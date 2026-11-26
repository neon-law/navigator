//! Logical transport preserves genuine creation evidence without adopting the destination.
use super::*;
use futures::FutureExt as _;
use sqlx::postgres::PgSslMode;
use store::postgres::native_origin::{self, NativeDatabase, OriginError, TierBinding};
use uuid::Uuid;

#[tokio::test]
async fn genuinely_created_origin_roundtrips_as_evidence_without_admitting_logical_target() {
    let options = options();
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let operation = Uuid::new_v4();
    let created = native_origin::create(options.clone(), operation, tier())
        .await
        .unwrap();
    // Track only resources whose exclusive CREATE actually succeeded.
    let mut owned = vec![(format!("navigator_native_{}", operation.simple()), None)];
    let result = std::panic::AssertUnwindSafe(async {
        let native = created.initialize().await.unwrap();
        owned[0].1 = Some(native.postgres().pool().clone());
        assert!(native_origin::admits(native.postgres()).await.unwrap());
        let source_origin = origin_row(native.postgres()).await;
        roundtrip(&admin, &native, &mut owned, None).await;
        assert_eq!(origin_row(native.postgres()).await, source_origin);
        assert!(native_origin::admits(native.postgres()).await.unwrap());
    })
    .catch_unwind()
    .await;
    cleanup(admin, owned, result).await;
}

#[tokio::test]
async fn real_upgrade_intent_and_completion_roundtrip_without_fresh_restamping() {
    let options = options();
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let creation = Uuid::new_v4();
    let created = native_origin::create(options, creation, tier())
        .await
        .unwrap();
    let mut owned = vec![(format!("navigator_native_{}", creation.simple()), None)];
    let result = std::panic::AssertUnwindSafe(async {
        let native = created.initialize().await.unwrap();
        owned[0].1 = Some(native.postgres().pool().clone());
        let original = origin_row(native.postgres()).await;
        let before: i64 =
            sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations WHERE success")
                .fetch_one(native.postgres().pool())
                .await
                .unwrap();
        // Run the actual fixed upgrade and SQLx migration, then cancel only
        // while completion is genuinely blocked in the owned fixture.
        let operation = store::test_support::native_upgrade_pending(&native)
            .await
            .unwrap();
        let after: i64 =
            sqlx::query_scalar("SELECT count(*) FROM public._sqlx_migrations WHERE success")
                .fetch_one(native.postgres().pool())
                .await
                .unwrap();
        assert_eq!(after, before + 1);
        let pending = upgrade_rows(native.postgres()).await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["upgrade_operation"], operation.to_string());
        assert_eq!(pending[0]["creation_operation"], creation.to_string());
        assert!(pending[0]["completed_at"].is_null());
        assert!(native_origin::admits(native.postgres()).await.is_err());
        roundtrip(&admin, &native, &mut owned, Some(false)).await;
        assert_eq!(upgrade_rows(native.postgres()).await, pending);
        store::test_support::native_upgrade_complete(&native, operation)
            .await
            .unwrap();
        let completed = upgrade_rows(native.postgres()).await;
        assert_eq!(completed.len(), 1);
        assert!(!completed[0]["completed_at"].is_null());
        let mut immutable_pending = pending[0].clone();
        let mut immutable_completed = completed[0].clone();
        immutable_pending
            .as_object_mut()
            .unwrap()
            .remove("completed_at");
        immutable_completed
            .as_object_mut()
            .unwrap()
            .remove("completed_at");
        assert_eq!(immutable_completed, immutable_pending);
        // This fixture deliberately advances beyond this release. Even actual
        // completion must not make the older binary eligible to run effects.
        assert!(native_origin::admits(native.postgres()).await.is_err());
        roundtrip(&admin, &native, &mut owned, Some(true)).await;
        assert_eq!(upgrade_rows(native.postgres()).await, completed);
        assert_eq!(origin_row(native.postgres()).await, original);
    })
    .catch_unwind()
    .await;
    cleanup(admin, owned, result).await;
}

async fn upgrade_rows(pg: &PostgresDb) -> Vec<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT to_jsonb(u) FROM public.database_origin_upgrade u ORDER BY upgrade_operation",
    )
    .fetch_all(pg.pool())
    .await
    .unwrap()
}

fn options() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(store::test_support::postgres_test_port())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable)
        .options([("session_preload_libraries", "age")])
}

fn tier() -> TierBinding {
    TierBinding {
        tier_id: Uuid::new_v4(),
        namespace_uid: Uuid::new_v4(),
        cluster_name: "synthetic-archive".into(),
    }
}

async fn origin_row(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o")
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

async fn roundtrip(
    admin: &PgPool,
    native: &NativeDatabase,
    owned: &mut Vec<(String, Option<PgPool>)>,
    completed: Option<bool>,
) {
    let id = Uuid::new_v4().simple().to_string();
    let first = owned.len();
    for name in [
        format!("origin_restore_{id}"),
        format!("origin_forge_{id}"),
        format!("origin_forge_restore_{id}"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(admin)
            .await
            .unwrap();
        owned.push((name.clone(), None));
        let pool = PgPool::connect_with(admin.connect_options().as_ref().clone().database(&name))
            .await
            .unwrap();
        owned.last_mut().unwrap().1 = Some(pool);
    }
    let target = owned[first].1.as_ref().unwrap();
    let forge = owned[first + 1].1.as_ref().unwrap();
    let forge_target = owned[first + 2].1.as_ref().unwrap();
    sqlx::raw_sql(store::postgres_schema::canonical_schema())
        .execute(target)
        .await
        .unwrap();
    restore::prepare_target_migration_ledger(native.postgres().pool(), target).await;
    for pool in [forge, forge_target] {
        sqlx::raw_sql(FORGE_SCHEMA).execute(pool).await.unwrap();
    }
    sqlx::query("INSERT INTO forge.repository VALUES(7,'synthetic-origin',true,decode('00ff','hex'),'{\"origin\":true}'::jsonb)")
        .execute(forge).await.unwrap();
    let nav_manifest = manifest(native.postgres().pool()).await;
    let forge_manifest = manifest(forge).await;
    let directory = tempfile::tempdir().unwrap();
    let storage = ZeroDelete {
        inner: FsStorage::new(directory.path()).await.unwrap(),
    };
    let base = "memory://database-origin";
    let paired = export_pair(
        source("navigator", native.postgres().pool(), &nav_manifest),
        source("forgejo", forge, &forge_manifest),
        &storage,
        base,
    )
    .await
    .unwrap();
    assert!(!paired.cross_database_atomic);
    assert_eq!(
        paired.databases[0].recovery_plan.is_some(),
        completed.is_none(),
        "a future upgrade is archived without authorizing native activation"
    );
    if completed.is_some() {
        assert!(matches!(
            store::postgres::native_origin::recovery::RecoveryInput::from_archive(
                Uuid::new_v4(),
                serde_json::to_vec(&paired).unwrap(),
            ),
            Err(store::postgres::native_origin::OriginError::Recovery)
        ));
    }
    let target_pg = PostgresDb::connect(target.connect_options().as_ref().clone())
        .await
        .unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        restore::restore_pair(&paired, &storage, base, target, forge_target, &target_pg).await;
        restore::assert_database_equal(&paired.databases[0], native.postgres().pool(), target)
            .await;
        restore::assert_database_equal(&paired.databases[1], forge, forge_target).await;
        assert_restored_provenance(native, &target_pg, completed).await;
    })
    .catch_unwind()
    .await;
    target_pg.pool().close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn assert_restored_provenance(
    native: &NativeDatabase,
    target_pg: &PostgresDb,
    completed: Option<bool>,
) {
    let target = target_pg.pool();
    assert_eq!(
        origin_row(target_pg).await,
        origin_row(native.postgres()).await
    );
    let upgrades = upgrade_rows(target_pg).await;
    assert_eq!(upgrades, upgrade_rows(native.postgres()).await);
    match completed {
        None => assert!(upgrades.is_empty()),
        Some(expected) => {
            assert_eq!(upgrades.len(), 1);
            assert_eq!(!upgrades[0]["completed_at"].is_null(), expected);
            for statement in [
                "UPDATE public.database_origin_upgrade SET authority_contract='[]'::jsonb",
                "DELETE FROM public.database_origin_upgrade",
            ] {
                assert!(sqlx::query(statement).execute(target).await.is_err());
            }
            if expected {
                assert!(sqlx::query(
                    "UPDATE public.database_origin_upgrade SET completed_at=now()"
                )
                .execute(target)
                .await
                .is_err());
            }
            assert_eq!(upgrade_rows(target_pg).await, upgrades);
        }
    }
    assert!(matches!(
        native_origin::admits(target_pg).await,
        Err(OriginError::Authority)
    ));
    let ran = std::sync::atomic::AtomicBool::new(false);
    assert!(PostgresDb::with_application_db(target_pg.clone(), async {
        ran.store(true, std::sync::atomic::Ordering::SeqCst);
    })
    .await
    .is_err());
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    let (name, oid, comment): (String, i64, Option<String>) = sqlx::query_as(
        "SELECT datname::text,oid::bigint,shobj_description(oid,'pg_database') FROM pg_database WHERE datname=current_database()",
    ).fetch_one(target).await.unwrap();
    assert_ne!(name, native.database());
    let source_oid: i64 = sqlx::query_scalar("SELECT database_oid FROM database_origin")
        .fetch_one(target)
        .await
        .unwrap();
    assert_ne!(oid, source_oid);
    assert!(
        comment.is_none(),
        "restoration must not stamp creation evidence"
    );
    for statement in [
        "UPDATE public.database_origin SET cluster_name='changed'",
        "DELETE FROM public.database_origin",
    ] {
        assert!(sqlx::query(statement).execute(target).await.is_err());
    }
}

async fn cleanup(
    admin: PgPool,
    owned: Vec<(String, Option<PgPool>)>,
    result: Result<(), Box<dyn std::any::Any + Send>>,
) {
    let mut cleanup_failed = false;
    for (name, pool) in owned {
        if let Some(pool) = pool {
            pool.close().await;
        }
        cleanup_failed |= sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
            .execute(&admin)
            .await
            .is_err();
    }
    admin.close().await;
    if let Err(panic) = result {
        if cleanup_failed {
            eprintln!("owned origin archive cleanup also failed");
        }
        std::panic::resume_unwind(panic);
    }
    assert!(!cleanup_failed, "owned origin archive cleanup failed");
}
