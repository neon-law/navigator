//! The window between a step's effect and its checkpoint.
//!
//! A step performs two synthetic effects, then the worker is shut down before
//! the step returns, so no checkpoint commits. Recovery re-runs the step. The
//! unkeyed effect repeats; the write keyed by `effect_key` lands once.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use workflows::dbos_engine;

use crate::support::{database, APP_VERSION, DEADLINE};

const WORKFLOW_ID: &str = "synthetic-effect-1";

/// The step body: one unkeyed effect (a counter) and one keyed write. The
/// first attempt then holds before returning, so its checkpoint never commits.
#[derive(Clone)]
struct Probe {
    attempts: Arc<AtomicU32>,
    keys: Arc<Mutex<Vec<String>>>,
    reached: Arc<Notify>,
    pool: sqlx::PgPool,
}

impl Probe {
    async fn effect(self) -> dbos::Result<()> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
        let key = dbos_engine::effect_key().expect("inside a step");
        self.keys.lock().expect("keys lock").push(key.clone());
        sqlx::query(
            "INSERT INTO synthetic_effect (effect_key) VALUES ($1) \
             ON CONFLICT (effect_key) DO NOTHING",
        )
        .bind(&key)
        .execute(&self.pool)
        .await
        .expect("keyed write");
        if attempt == 1 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_keyed_write_lands_once_when_recovery_repeats_its_step() {
    let Some(db) = database().await else {
        return;
    };
    sqlx::query("CREATE TABLE synthetic_effect (effect_key text PRIMARY KEY)")
        .execute(&db.pool)
        .await
        .expect("create the synthetic effect table");

    let probe = Probe {
        attempts: Arc::new(AtomicU32::new(0)),
        keys: Arc::new(Mutex::new(Vec::new())),
        reached: Arc::new(Notify::new()),
        pool: db.pool.clone(),
    };

    let dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = {
        let probe = probe.clone();
        dbos.register_workflow("synthetic-effect", move |()| {
            let probe = probe.clone();
            async move {
                dbos::step("effect", || probe.clone().effect()).await?;
                Ok::<_, dbos::Error>(())
            }
        })
        .expect("register the workflow")
    };
    dbos.launch().await.expect("launch");

    let running = tokio::spawn(async move {
        workflow
            .run_with(
                (),
                dbos::RunOptions {
                    workflow_id: Some(WORKFLOW_ID),
                    ..Default::default()
                },
            )
            .await
    });
    tokio::time::timeout(DEADLINE, probe.reached.notified())
        .await
        .expect("the step never performed its effects");
    dbos.shutdown().await;
    let interrupted = running.await.expect("the task panicked");
    assert!(
        matches!(interrupted, Err(dbos::Error::Interrupted { .. })),
        "shutdown interrupts the step: {interrupted:?}"
    );

    // The relaunch recovers the pending workflow and re-runs the step.
    dbos.launch().await.expect("relaunch");
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<(), dbos::EngineOnly>(WORKFLOW_ID)
            .expect("retrieve the workflow")
            .result(),
    )
    .await
    .expect("the recovered workflow never finished")
    .expect("the recovered workflow succeeds");
    dbos.shutdown().await;

    assert_eq!(
        probe.attempts.load(Ordering::SeqCst),
        2,
        "the unkeyed effect repeats: the step had no checkpoint"
    );
    let keys = probe.keys.lock().expect("keys lock").clone();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0], keys[1],
        "the effect key is stable across the re-run"
    );
    assert!(
        keys[0].starts_with(&format!("{WORKFLOW_ID}/")),
        "{}",
        keys[0]
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM synthetic_effect")
        .fetch_one(&db.pool)
        .await
        .expect("count keyed writes");
    assert_eq!(rows, 1, "the keyed write lands once");
}
