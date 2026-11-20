//! Domain-linked recovery over the actual canonical schema and SDK-created history.
//! This SDK probe exercises native receipt admission, not the EmailSummary controller.

use super::*;
use futures::FutureExt as _;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use store::email_receipts::summary_recovery::{self, DeliveryRule};
use store::project_slack_deliveries::{
    self,
    reconciliation::{self, Decision, Request},
};
use uuid::Uuid;

#[derive(Clone, Copy)]
struct Domain {
    receipt: Uuid,
    operation: Uuid,
    epoch: Uuid,
    retained: Uuid,
    unresolved: Uuid,
    deleted: Uuid,
}

#[derive(Debug, Clone, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("native archive admission failed")]
struct ProbeError;

#[tokio::test]
async fn canonical_native_owners_and_deleted_project_audit_restore_with_real_sdk_checkpoints() {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
        return;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        );
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    // This constructor alone supplies known-fresh native ownership BEFORE any data.
    let fixture = store::test_support::PostgresFixture::create(options)
        .await
        .unwrap();
    let mut owned = Vec::new();
    let result=std::panic::AssertUnwindSafe(async {
        let id=Uuid::now_v7().simple().to_string();
        for name in [format!("native_restore_{id}"),format!("native_forge_{id}"),format!("native_restore_forge_{id}")] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}"))).execute(&admin).await.unwrap();
            owned.push((name.clone(), None));
            let pool=PgPool::connect_with(admin.connect_options().as_ref().clone().database(&name)).await.unwrap();
            owned.last_mut().unwrap().1=Some(pool);
        }
        let target=owned[0].1.as_ref().unwrap();let forge=owned[1].1.as_ref().unwrap();let forge_target=owned[2].1.as_ref().unwrap();
        // Schema-only target: no invented ownership epoch, fixture history or workflow rows.
        sqlx::raw_sql(store::postgres_schema::canonical_schema()).execute(target).await.unwrap();
        for pool in [forge,forge_target] {sqlx::raw_sql(FORGE_SCHEMA).execute(pool).await.unwrap();}
        sqlx::query("INSERT INTO forge.repository VALUES(1,'synthetic',true,decode('00ff','hex'),'{\"native\":true}'::jsonb)")
            .execute(forge).await.unwrap();
        roundtrip(fixture.postgres(),target,forge,forge_target,&port).await;
    }).catch_unwind().await;
    let mut cleanup_failed = false;
    for (name, pool) in owned {
        if let Some(pool) = pool {
            pool.close().await;
        }
        if sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
            .execute(&admin)
            .await
            .is_err()
        {
            cleanup_failed = true;
        }
    }
    cleanup_failed |= fixture.close().await.is_err();
    admin.close().await;
    if let Err(panic) = result {
        if cleanup_failed {
            eprintln!("owned recovery fixture cleanup also failed");
        }
        std::panic::resume_unwind(panic);
    }
    assert!(!cleanup_failed, "owned recovery fixture cleanup failed");
}

async fn parents(pg: &PostgresDb) -> (Uuid, Uuid, String) {
    let (person, firm, entity, kind, jurisdiction) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query(
        "INSERT INTO person(id,name,email,role) VALUES($1,'Synthetic Operator',$2,'owner')",
    )
    .bind(person)
    .bind(format!("{person}@example.test"))
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES($1,$2)")
        .bind(kind)
        .bind(kind.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES($1,$2)")
        .bind(jurisdiction)
        .bind(jurisdiction.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO firm(id,name,status,slug,entity_id) VALUES($1,'Synthetic Practice','active',$2,$3)")
        .bind(firm).bind(firm.to_string()).bind(entity).execute(pg.pool()).await.unwrap();
    let brand = format!("archive-{}", Uuid::now_v7().simple());
    sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES('Synthetic Brand',$1,$2)")
        .bind(&brand)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    (person, entity, brand)
}

async fn seed(pg: &PostgresDb) -> Domain {
    let (person, entity, brand) = parents(pg).await;
    let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM brand WHERE brand_key=$1")
        .bind(&brand)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let mut projects = Vec::new();
    for _ in 0..3 {
        let project = store::postgres::projects::create_on(
            pg,
            &store::projects::NewProject {
                code: Uuid::now_v7().to_string(),
                name: "Synthetic Archive Project".into(),
                status: "open".into(),
                brand: brand.clone(),
                entity_id: entity,
                firm_id: Some(firm),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // Controlled unresolved evidence fixture; this is not a captured-source attestation.
        sqlx::query("UPDATE project_slack_handover SET client_view_disposition='unresolved' WHERE project_id=$1")
            .bind(project.id).execute(pg.pool()).await.unwrap();
        projects.push(project.id);
    }
    sqlx::query("INSERT INTO project_status_transition(project_id,to_status) VALUES($1,'open')")
        .bind(projects[1])
        .execute(pg.pool())
        .await
        .unwrap();
    for project in &projects[..2] {
        reconciliation::reconcile(
            pg,
            Request {
                operation_id: Uuid::now_v7(),
                project_id: *project,
                ownership_epoch: epoch,
                actor_person_id: person,
                evidence_reference: Uuid::now_v7(),
                decision: Decision::Posted,
            },
        )
        .await
        .unwrap();
    }
    let source = store::surreal::SurrealDb::uninitialized();
    let receipt = PostgresDb::with_test_db(pg.clone(), async {
        store::projects::delete_project_with_surreal(&source, projects[0])
            .await
            .unwrap();
        let receipt = store::email_receipts::ensure(
            &source,
            &store::email_receipts::NewEmailReceipt {
                receiving_mailbox: "archive@example.test",
                deployment: "synthetic",
                firm_id: Some(firm),
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "native-archive-digest",
                source_message_id: None,
                archive_key: "synthetic/mail.eml",
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap()
        .receipt
        .id;
        store::email_deliveries::ensure(&source, receipt, "C-SYNTHETIC")
            .await
            .unwrap();
        store::email_deliveries::mark_confirmed(&source, receipt, "C-SYNTHETIC", "synthetic-post")
            .await
            .unwrap();
        receipt
    })
    .await;
    Domain {
        receipt,
        operation: Uuid::now_v7(),
        epoch,
        retained: projects[1],
        unresolved: projects[2],
        deleted: projects[0],
    }
}

async fn sdk(
    pg: &PostgresDb,
    port: &str,
    domain: Option<Domain>,
    replay: Option<Domain>,
    calls: Arc<AtomicUsize>,
) {
    let database: String = sqlx::query_scalar("SELECT current_database()::text")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let url = format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}");
    let mut config = dbos::Config::new("native-ownership-archive", &url);
    config.schema = "dbos".into();
    config.app_version = Some("native-ownership-archive-v1".into());
    let runtime = dbos::DBOS::new(config);
    let native = pg.clone();
    let workflow = runtime
        .register_workflow(
            "native-archive-recovery-probe",
            move |(receipt, operation): (Uuid, Uuid)| {
                let native = native.clone();
                let calls = Arc::clone(&calls);
                async move {
                    dbos::step("claim-native-receipt-owner", move || {
                        let native = native.clone();
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            summary_recovery::claim(
                                &native,
                                receipt,
                                operation,
                                None,
                                "native-archive-digest",
                                DeliveryRule::PreserveConfirmed,
                            )
                            .await
                            .map_err(|_| dbos::Error::Application(ProbeError))?;
                            Ok::<_, dbos::Error<ProbeError>>(operation)
                        }
                    })
                    .await
                }
            },
        )
        .unwrap();
    runtime.launch().await.unwrap();
    if let Some(domain) = domain.or(replay) {
        let id = format!("native-archive-recovery:{}", domain.operation);
        let result = workflow
            .run_with(
                (domain.receipt, domain.operation),
                dbos::RunOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await;
        runtime.shutdown().await;
        assert_eq!(result.unwrap(), domain.operation);
    } else {
        runtime.shutdown().await;
    }
}

async fn clear_sdk_target(pool: &PgPool) {
    let names: Vec<String> =
        sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname='dbos'")
            .fetch_all(pool)
            .await
            .unwrap();
    let names = names
        .iter()
        .map(|name| format!("dbos.\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    // Only the owned, schema-only target; no source or fabricated SDK history.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("TRUNCATE {names} CASCADE")))
        .execute(pool)
        .await
        .unwrap();
}

async fn roundtrip(
    source_pg: &PostgresDb,
    target: &PgPool,
    forge: &PgPool,
    forge_target: &PgPool,
    port: &str,
) {
    let target_pg = PostgresDb::connect(target.connect_options().as_ref().clone())
        .await
        .unwrap();
    let result = std::panic::AssertUnwindSafe(roundtrip_on(
        source_pg,
        &target_pg,
        forge,
        forge_target,
        port,
    ))
    .catch_unwind()
    .await;
    target_pg.pool().close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn roundtrip_on(
    source_pg: &PostgresDb,
    target_pg: &PostgresDb,
    forge: &PgPool,
    forge_target: &PgPool,
    port: &str,
) {
    prepare_target_migration_ledger(source_pg.pool(), target_pg.pool()).await;
    let domain = seed(source_pg).await;
    let calls = Arc::new(AtomicUsize::new(0));
    sdk(source_pg, port, Some(domain), None, Arc::clone(&calls)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        summary_recovery::current_operation(source_pg, domain.receipt)
            .await
            .unwrap(),
        Some(domain.operation)
    );
    sdk(target_pg, port, None, None, Arc::new(AtomicUsize::new(0))).await;
    clear_sdk_target(target_pg.pool()).await;
    assert!(
        store::durable_object_handover::require_native_ownership(target_pg)
            .await
            .is_err()
    );
    assert!(matches!(
        project_slack_deliveries::admission(
            target_pg,
            domain.retained,
            "client_view",
            "client-view"
        )
        .await,
        Err(project_slack_deliveries::DeliveryError::HandoverRequired)
    ));
    let nav = manifest(source_pg.pool()).await;
    let forge_manifest = manifest(forge).await;
    let dir = tempfile::tempdir().unwrap();
    let storage = ZeroDelete {
        inner: FsStorage::new(dir.path()).await.unwrap(),
    };
    let base = "memory://native-ownership";
    let paired = export_pair(
        source("navigator", source_pg.pool(), &nav),
        source("forgejo", forge, &forge_manifest),
        &storage,
        base,
    )
    .await
    .unwrap();
    assert!(
        !paired.cross_database_atomic,
        "two databases do not share a recovery point"
    );
    restore::restore_pair(
        &paired,
        &storage,
        base,
        target_pg.pool(),
        forge_target,
        target_pg,
    )
    .await;
    require_scope(target_pg, Family::Satellites).await.unwrap();
    restore::assert_database_equal(&paired.databases[0], source_pg.pool(), target_pg.pool()).await;
    restore::assert_database_equal(&paired.databases[1], forge, forge_target).await;
    assert_native_projection(target_pg, domain).await;
    reject_target_only_fixed_reference(&paired.databases[0], source_pg.pool(), target_pg.pool())
        .await;
    check_incoherent_epoch(target_pg, domain).await;
    let replayed = Arc::new(AtomicUsize::new(0));
    sdk(target_pg, port, None, Some(domain), Arc::clone(&replayed)).await;
    assert_eq!(
        replayed.load(Ordering::SeqCst),
        0,
        "restored real SDK checkpoint/result must not rerun native admission"
    );
    assert_native_projection(target_pg, domain).await;
    assert_identity_continuity(source_pg.pool(), target_pg.pool()).await;
}

async fn assert_native_projection(pg: &PostgresDb, domain: Domain) {
    for table in [
        "durable_object_handover",
        "durable_object_ownership",
        "project_slack_handover",
        "project_slack_delivery",
        "project_slack_delivery_reconciliation",
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM {table}")))
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert!(count > 0, "the roundtrip must exercise {table}");
    }
    assert_eq!(
        summary_recovery::current_operation(pg, domain.receipt)
            .await
            .unwrap(),
        Some(domain.operation)
    );
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(epoch, domain.epoch);
    assert_eq!(
        project_slack_deliveries::admission(pg, domain.retained, "client_view", "client-view")
            .await
            .unwrap(),
        project_slack_deliveries::Admission::Posted
    );
    assert!(matches!(
        project_slack_deliveries::admission(pg, domain.unresolved, "client_view", "client-view")
            .await,
        Err(project_slack_deliveries::DeliveryError::Unresolved)
    ));
    let deleted: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1)")
        .bind(domain.deleted)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(
        !deleted,
        "restoring audit must not recreate its deleted Project"
    );
    let operation: Uuid = sqlx::query_scalar(
        "SELECT operation_id FROM project_slack_delivery_reconciliation WHERE project_id=$1",
    )
    .bind(domain.deleted)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    for sql in ["UPDATE project_slack_delivery_reconciliation SET decision='verified_not_posted' WHERE operation_id=$1","DELETE FROM project_slack_delivery_reconciliation WHERE operation_id=$1"] {
        assert!(sqlx::query(sql).bind(operation).execute(pg.pool()).await.is_err(),"immutable trigger must survive restoration");
    }
}

async fn check_incoherent_epoch(pg: &PostgresDb, domain: Domain) {
    sqlx::query("UPDATE project_slack_handover SET ownership_epoch=$2 WHERE project_id=$1")
        .bind(domain.retained)
        .bind(Uuid::now_v7())
        .execute(pg.pool())
        .await
        .unwrap();
    assert!(matches!(
        project_slack_deliveries::admission(pg, domain.retained, "client_view", "client-view")
            .await,
        Err(project_slack_deliveries::DeliveryError::HandoverRequired)
    ));
    // Correct only this owned negative-fixture mutation before replaying the restored SDK.
    sqlx::query("UPDATE project_slack_handover SET ownership_epoch=$2 WHERE project_id=$1")
        .bind(domain.retained)
        .bind(domain.epoch)
        .execute(pg.pool())
        .await
        .unwrap();
}

async fn reject_target_only_fixed_reference(
    manifest: &archives::coverage::DatabaseManifest,
    source: &PgPool,
    target: &PgPool,
) {
    let srid:i32=sqlx::query_scalar("SELECT candidate FROM generate_series(990000,990100) AS candidate WHERE NOT EXISTS(SELECT 1 FROM spatial_ref_sys WHERE srid=candidate) LIMIT 1")
        .fetch_one(target).await.unwrap();
    sqlx::query("INSERT INTO spatial_ref_sys(srid,auth_name,auth_srid,srtext,proj4text) VALUES($1,'synthetic',1,'synthetic target-only reference','')")
        .bind(srid).execute(target).await.unwrap();
    let refused =
        std::panic::AssertUnwindSafe(restore::assert_database_equal(manifest, source, target))
            .catch_unwind()
            .await;
    sqlx::query("DELETE FROM spatial_ref_sys WHERE srid=$1")
        .bind(srid)
        .execute(target)
        .await
        .unwrap();
    assert!(
        refused.is_err(),
        "fixed-reference conflict handling must never certify target-only rows"
    );
    restore::assert_database_equal(manifest, source, target).await;
}

async fn prepare_target_migration_ledger(source: &PgPool, target: &PgPool) {
    use sqlx::migrate::Migrate as _;
    let present: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(source)
            .await
            .unwrap();
    if present {
        let mut connection = target.acquire().await.unwrap();
        // SQLx0.9 creates only the ledger shape; no applied versions are invented.
        connection
            .ensure_migrations_table("public._sqlx_migrations")
            .await
            .unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(
            applied, 0,
            "schema-only target must not fabricate applied migrations"
        );
    }
}

async fn assert_identity_continuity(source: &PgPool, restored: &PgPool) {
    let next_sql = "SELECT nextval(pg_get_serial_sequence('project_status_transition','sequence'))";
    let source_next: i64 = sqlx::query_scalar(next_sql)
        .fetch_one(source)
        .await
        .unwrap();
    let restored_next: i64 = sqlx::query_scalar(next_sql)
        .fetch_one(restored)
        .await
        .unwrap();
    assert_eq!(
        restored_next, source_next,
        "restored identity must continue the source counter"
    );
    let highest_restored: i64 =
        sqlx::query_scalar("SELECT max(sequence) FROM project_status_transition")
            .fetch_one(restored)
            .await
            .unwrap();
    assert!(
        restored_next > highest_restored,
        "the next identity must not collide with a restored row"
    );
}
