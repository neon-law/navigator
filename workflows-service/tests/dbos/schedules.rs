//! Schedule shutdown while PostgreSQL is holding a DBOS workflow start.

use std::sync::Arc;
use std::time::Duration;

use workflows::dbos_engine::{self, heartbeat, Executor, Settings};
use workflows::CapturingNotifier;

use crate::support::{database, TestDatabase, APP_VERSION, DEADLINE};

#[tokio::test]
async fn blocked_start_times_out_and_graceful_shutdown_recovers_the_same_slot_once() {
    let Some(db) = database().await else {
        return;
    };
    install_start_gate(&db).await;

    let mut lock = db
        .pool
        .begin()
        .await
        .expect("begin advisory-lock transaction");
    sqlx::query_scalar::<_, ()>("SELECT pg_advisory_xact_lock(11561156)")
        .fetch_one(&mut *lock)
        .await
        .expect("hold only the synthetic Heartbeat start");

    let settings = Settings::from_lookup(db.lookup()).expect("complete DBOS settings");

    let id = heartbeat::workflow_id(heartbeat::slot(chrono::Utc::now()));
    assert_start_lock_timeout(&db, &settings, &id).await;

    let executor = Executor::launch(&settings, Arc::new(CapturingNotifier::new()))
        .await
        .expect("launch executor with its real schedule task");
    wait_for_start_lock(&db.pool).await;

    tokio::time::timeout(Duration::from_secs(10), executor.shutdown())
        .await
        .expect("executor shutdown waited for PostgreSQL-blocked schedule start");
    lock.rollback()
        .await
        .expect("release the synthetic PostgreSQL lock");

    let row: Option<String> =
        sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(&id)
            .fetch_optional(&db.pool)
            .await
            .expect("check whether PostgreSQL accepted the start");
    assert_eq!(
        row, None,
        "cancelled start must not report or persist acceptance"
    );

    let recovered = dbos::DBOS::new(settings.config());
    let workflow = recovered
        .register_workflow("Heartbeat", |()| async { Ok::<_, dbos::Error>(()) })
        .expect("register recovered heartbeat");
    recovered.launch().await.expect("launch recovered slot");
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
        .expect("start after releasing lock");
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(&id)
            .fetch_one(&db.pool)
            .await
            .expect("count the recovered slot");
    assert_eq!(rows, 1, "the deterministic slot has one recorded run");
    recovered.shutdown().await;
}

async fn install_start_gate(db: &TestDatabase) {
    // Initialize DBOS once before holding the workflow-status table, so
    // schema migrations are not part of this blocked-start scenario.
    let bootstrap = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    bootstrap.launch().await.expect("initialize DBOS schema");
    bootstrap.shutdown().await;
    sqlx::query(
        "CREATE FUNCTION hold_synthetic_heartbeat_start() RETURNS trigger \
         LANGUAGE plpgsql AS $$ BEGIN \
         IF NEW.name = 'Heartbeat' THEN PERFORM pg_advisory_xact_lock(11561156); END IF; \
         RETURN NEW; END $$",
    )
    .execute(&db.pool)
    .await
    .expect("install synthetic start gate");
    sqlx::query(
        "CREATE TRIGGER hold_synthetic_heartbeat_start BEFORE INSERT \
         ON dbos.workflow_status FOR EACH ROW \
         EXECUTE FUNCTION hold_synthetic_heartbeat_start()",
    )
    .execute(&db.pool)
    .await
    .expect("attach synthetic start gate");
}

async fn assert_start_lock_timeout(db: &TestDatabase, settings: &Settings, id: &str) {
    // Exercise the production DBOS URL options directly and require PostgreSQL's
    // lock timeout (55P03), rather than treating any start failure as success.
    let probe = dbos::DBOS::new(settings.config());
    let workflow = probe
        .register_workflow("Heartbeat", |()| async { Ok::<_, dbos::Error>(()) })
        .expect("register synthetic heartbeat");
    probe.launch().await.expect("launch lock-timeout probe");
    let start_id = id.to_owned();
    let start = tokio::spawn(async move {
        workflow
            .start_with(
                (),
                dbos::StartOptions {
                    workflow_id: Some(&start_id),
                    ..dbos::StartOptions::default()
                },
            )
            .await
    });
    wait_for_start_lock(&db.pool).await;
    let error = tokio::time::timeout(DEADLINE, start)
        .await
        .expect("system DB lock timeout did not bound start")
        .expect("start task panicked")
        .expect_err("blocked workflow start unexpectedly succeeded");
    assert!(
        format!("{error:?}").contains("55P03"),
        "expected lock_not_available: {error:?}"
    );
    probe.shutdown().await;
}

async fn wait_for_start_lock(pool: &sqlx::PgPool) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let held: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                 AND query ILIKE '%workflow_status%')",
            )
            .fetch_one(pool)
            .await
            .expect("inspect synthetic PostgreSQL activity");
            if held {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("synthetic start never waited on the advisory lock");
}
