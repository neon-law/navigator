use super::*;
use sha2::Digest as _;
use sqlx::postgres::PgSslMode;
use sqlx::SqlSafeStr;

fn options() -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(crate::test_support::postgres_test_port())
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
        cluster_name: "synthetic-native-tier".into(),
    }
}

async fn cleanup(options: PgConnectOptions, pg: PostgresDb, operation: Uuid) {
    pg.pool().close().await;
    let admin = PostgresDb::connect(options).await.unwrap();
    // Only the exact UUID name this case created, never the shared test database.
    let name = database_name(operation).unwrap();
    sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
}

#[test]
fn invalid_tier_and_nil_operation_fail_before_connecting() {
    let mut binding = tier();
    binding.namespace_uid = Uuid::nil();
    assert!(binding.validate().is_err());
    binding = tier();
    binding.cluster_name = "wrong'; DROP DATABASE x;--".into();
    assert!(binding.validate().is_err());
    assert!(database_name(Uuid::nil()).is_err());
}

#[tokio::test]
async fn actual_new_database_acquires_native_authority_without_any_capture_receipts() {
    let options = options();
    let operation = Uuid::new_v4();
    let binding = tier();
    let created = create(options.clone(), operation, binding.clone())
        .await
        .unwrap();
    assert!(matches!(
        admits(&created.postgres).await,
        Err(OriginError::Recovery)
    ));
    let native = created.initialize().await.unwrap();
    assert!(admits(native.postgres()).await.unwrap());
    native
        .postgres()
        .require_application_history()
        .await
        .unwrap();
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_run")
        .fetch_one(native.postgres().pool())
        .await
        .unwrap();
    assert_eq!(
        history, 0,
        "native origin never fabricates captured-source history"
    );
    assert_eq!(native.tier(), &binding);
    assert_eq!(native.operation(), operation);
    assert_eq!(native.database(), database_name(operation).unwrap());
    let mutation = sqlx::query("UPDATE database_origin SET cluster_name='changed'")
        .execute(native.postgres().pool())
        .await;
    assert!(mutation.is_err(), "initial authority is immutable");
    let resumed = resume(options.clone(), operation, binding).await.unwrap();
    assert!(admits(resumed.postgres()).await.unwrap());
    resumed.postgres.pool().close().await;
    cleanup(options, native.postgres, operation).await;
}

#[tokio::test]
async fn concurrent_creators_never_adopt_an_existing_empty_resource() {
    let options = options();
    let operation = Uuid::new_v4();
    let binding = tier();
    let (left, right) = tokio::join!(
        create(options.clone(), operation, binding.clone()),
        create(options.clone(), operation, binding.clone())
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "only successful CREATE mints a capability"
    );
    let ((Ok(created), Err(OriginError::Creation)) | (Err(OriginError::Creation), Ok(created))) =
        (left, right)
    else {
        panic!("one unique creation and one refused duplicate required");
    };
    assert!(matches!(
        resume(options.clone(), operation, binding.clone()).await,
        Err(OriginError::Recovery)
    ));
    assert!(matches!(
        create(options.clone(), operation, binding).await,
        Err(OriginError::Creation)
    ));
    assert!(matches!(
        admits(&created.postgres).await,
        Err(OriginError::Recovery)
    ));
    cleanup(options, created.postgres, operation).await;
}

#[tokio::test]
async fn resume_rejects_foreign_tier_schema_drift_and_mixed_capture_history() {
    let options = options();
    let operation = Uuid::new_v4();
    let binding = tier();
    let native = create(options.clone(), operation, binding.clone())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let mut foreign = binding.clone();
    foreign.namespace_uid = Uuid::new_v4();
    assert!(matches!(
        resume(options.clone(), operation, foreign).await,
        Err(OriginError::Identity)
    ));
    sqlx::query("INSERT INTO mirror_backfill_run(id,batch_size,status,scope) VALUES($1,1,'running','global')")
        .bind(Uuid::new_v4()).execute(native.postgres().pool()).await.unwrap();
    assert!(matches!(
        admits(native.postgres()).await,
        Err(OriginError::Authority)
    ));
    assert!(native
        .postgres()
        .require_application_history()
        .await
        .is_err());
    sqlx::query("DELETE FROM mirror_backfill_run")
        .execute(native.postgres().pool())
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM _sqlx_migrations)")
        .execute(native.postgres().pool()).await.unwrap();
    assert!(matches!(
        admits(native.postgres()).await,
        Err(OriginError::Authority)
    ));
    cleanup(options, native.postgres, operation).await;
}

#[tokio::test]
async fn restoring_only_origin_rows_cannot_attest_another_database() {
    let options = options();
    let source_operation = Uuid::new_v4();
    let source = create(options.clone(), source_operation, tier())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let origin: serde_json::Value = sqlx::query_scalar("SELECT to_jsonb(o) FROM database_origin o")
        .fetch_one(source.postgres().pool())
        .await
        .unwrap();
    let target_operation = Uuid::new_v4();
    let target = create(options.clone(), target_operation, tier())
        .await
        .unwrap();
    target.postgres.migrate_embedded(None).await.unwrap();
    sqlx::query(
        "INSERT INTO database_origin SELECT (jsonb_populate_record(NULL::database_origin,$1)).*",
    )
    .bind(origin)
    .execute(target.postgres.pool())
    .await
    .unwrap();
    assert!(matches!(
        admits(&target.postgres).await,
        Err(OriginError::Authority)
    ));
    assert!(target.postgres.require_application_history().await.is_err());
    cleanup(options.clone(), target.postgres, target_operation).await;
    cleanup(options, source.postgres, source_operation).await;
}

async fn assert_runtime_metadata_read_only(runtime: &PostgresDb) {
    for statement in [
        "INSERT INTO database_origin SELECT * FROM database_origin WHERE false",
        "UPDATE database_origin SET format_version=format_version WHERE false",
        "DELETE FROM database_origin WHERE false",
        "TRUNCATE database_origin",
        "INSERT INTO database_origin_upgrade SELECT * FROM database_origin_upgrade WHERE false",
        "UPDATE database_origin_upgrade SET completed_at=now() WHERE false",
        "DELETE FROM database_origin_upgrade WHERE false",
        "TRUNCATE database_origin_upgrade",
        "INSERT INTO durable_object_handover SELECT * FROM durable_object_handover WHERE false",
        "UPDATE durable_object_handover SET capture_version=capture_version WHERE false",
        "DELETE FROM durable_object_handover WHERE false",
        "TRUNCATE durable_object_handover",
        "INSERT INTO durable_object_ownership SELECT * FROM durable_object_ownership WHERE false",
        "UPDATE durable_object_ownership SET ownership_epoch=ownership_epoch WHERE false",
        "DELETE FROM durable_object_ownership WHERE false",
        "TRUNCATE durable_object_ownership",
        "SELECT public.protect_database_origin_runtime(current_user::regrole)",
    ] {
        let error = sqlx::query(statement)
            .execute(runtime.pool())
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("42501"),
            "runtime privileges must refuse before a trigger or row constraint"
        );
    }
}

async fn assert_runtime_project_delivery(pg: &PostgresDb, runtime: &PostgresDb) {
    let kind = Uuid::new_v4();
    let jurisdiction = Uuid::new_v4();
    let entity = Uuid::new_v4();
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
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES($1,'Synthetic native client',$2,$3)")
        .bind(entity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    let brand = format!("synthetic-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO brand(brand_key,name) VALUES($1,$2)")
        .bind(&brand)
        .bind(&brand)
        .execute(pg.pool())
        .await
        .unwrap();
    let source = crate::surreal::SurrealDb::uninitialized();
    let input = crate::projects::NewProject {
        code: format!("synthetic-{}", Uuid::new_v4().simple()),
        name: "Synthetic native matter".into(),
        status: "open".into(),
        entity_id: entity,
        brand,
        ..Default::default()
    };
    let project =
        PostgresDb::with_test_db(runtime.clone(), crate::projects::create(&source, &input))
            .await
            .unwrap();
    assert!(matches!(
        crate::project_slack_deliveries::admission(
            runtime,
            project.id,
            "client_view",
            "client-view"
        )
        .await
        .unwrap(),
        crate::project_slack_deliveries::Admission::Allowed
    ));
    crate::project_slack_deliveries::record_posted(
        runtime,
        project.id,
        "client_view",
        "client-view",
    )
    .await
    .unwrap();
    assert!(matches!(
        crate::project_slack_deliveries::admission(
            runtime,
            project.id,
            "client_view",
            "client-view"
        )
        .await
        .unwrap(),
        crate::project_slack_deliveries::Admission::Posted
    ));
}

#[tokio::test]
async fn actual_runtime_login_admits_without_ledger_read_or_origin_write_rights() {
    let options = options();
    let operation = Uuid::new_v4();
    let native = create(options.clone(), operation, tier())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let role = format!("native_origin_app_{}", Uuid::new_v4().simple());
    let pg = native.postgres();
    sqlx::query(AssertSqlSafe(format!("CREATE ROLE \"{role}\" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD 'synthetic'")))
        .execute(pg.pool()).await.unwrap();
    for statement in [
        format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO \"{role}\"",
            native.database()
        ),
        format!("GRANT USAGE ON SCHEMA public TO \"{role}\""),
        format!("GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA public TO \"{role}\""),
        format!("REVOKE ALL ON TABLE _sqlx_migrations FROM \"{role}\""),
    ] {
        sqlx::query(AssertSqlSafe(statement))
            .execute(pg.pool())
            .await
            .unwrap();
    }
    // This is the SAME policy routine called by the actual role initializer
    // after generic grants, not a parallel test-only privilege implementation.
    sqlx::query("SELECT public.protect_database_origin_runtime($1::text::regrole)")
        .bind(&role)
        .execute(pg.pool())
        .await
        .unwrap();
    let runtime_options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(
            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT")
                .unwrap()
                .parse()
                .unwrap(),
        )
        .username(&role)
        .password("synthetic")
        .database(&native.database())
        .ssl_mode(PgSslMode::Disable);
    let runtime = PostgresDb::connect(runtime_options).await.unwrap();
    let superuser: bool =
        sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname=current_user")
            .fetch_one(runtime.pool())
            .await
            .unwrap();
    assert!(!superuser);
    runtime.require_application_history().await.unwrap();
    assert!(sqlx::query("SELECT * FROM _sqlx_migrations")
        .fetch_all(runtime.pool())
        .await
        .is_err());
    assert_runtime_metadata_read_only(&runtime).await;
    // The normal Project path still initializes its own per-Project evidence,
    // and a native receipt still records a post without changing global ownership.
    assert_runtime_project_delivery(pg, &runtime).await;
    sqlx::query("UPDATE _sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM _sqlx_migrations)")
        .execute(pg.pool()).await.unwrap();
    assert!(runtime.require_application_history().await.is_err());
    sqlx::query("DELETE FROM _sqlx_migrations")
        .execute(pg.pool())
        .await
        .unwrap();
    let empty: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT public.database_origin_schema_contract()")
            .fetch_one(runtime.pool())
            .await
            .unwrap();
    assert!(empty.is_none());
    assert!(runtime.require_application_history().await.is_err());
    runtime.pool().close().await;
    sqlx::query(AssertSqlSafe(format!("DROP OWNED BY \"{role}\"")))
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query(AssertSqlSafe(format!("DROP ROLE \"{role}\"")))
        .execute(pg.pool())
        .await
        .unwrap();
    cleanup(options, native.postgres, operation).await;
}

#[tokio::test]
async fn raw_canonical_schema_accepts_origin_attestation_without_a_sqlx_ledger() {
    let options = options();
    let operation = Uuid::new_v4();
    let created = create(options.clone(), operation, tier()).await.unwrap();
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
        .execute(created.postgres.pool())
        .await
        .unwrap();
    sqlx::raw_sql(crate::postgres_schema::canonical_schema())
        .execute(created.postgres.pool())
        .await
        .unwrap();
    let origin_exists: bool =
        sqlx::query_scalar("SELECT to_regclass('public.database_origin') IS NOT NULL")
            .fetch_one(created.postgres.pool())
            .await
            .unwrap();
    assert!(
        origin_exists,
        "canonical schema must include Database Origin"
    );
    let ledger: bool =
        sqlx::query_scalar("SELECT to_regclass('public._sqlx_migrations') IS NOT NULL")
            .fetch_one(created.postgres.pool())
            .await
            .unwrap();
    assert!(
        !ledger,
        "raw canonical fixtures do not manufacture SQLx bookkeeping"
    );
    assert!(!admits(&created.postgres).await.unwrap());
    let unattested = sqlx::query("SELECT public.database_origin_schema_contract()")
        .execute(created.postgres.pool())
        .await
        .unwrap_err();
    assert_eq!(
        unattested
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("42P01")
    );
    cleanup(options, created.postgres, operation).await;
}

#[tokio::test]
async fn native_origin_fences_forced_source_replay_without_claiming_captured_history() {
    use crate::postgres_backfill::{
        self,
        family::{self, Family, ImportMode},
    };
    use crate::postgres_mirror::{self as mirror, MirrorKey, Mirrored};
    let options = options();
    let operation = Uuid::new_v4();
    let native = create(options.clone(), operation, tier())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let pg = native.postgres();
    let source = crate::surreal::SurrealDb::uninitialized();
    assert!(
        !mirror::backfilled(pg).await.unwrap(),
        "native creation is never relabeled captured-global history"
    );
    family::require_runtime_scopes(pg).await.unwrap();
    for selected in Family::all() {
        family::require_scope(pg, *selected).await.unwrap();
    }
    let person = PostgresDb::with_test_db(
        pg.clone(),
        crate::persons::create(
            &source,
            &crate::persons::NewPerson::with_role(
                "Synthetic native person",
                format!("synthetic-{}@example.com", Uuid::new_v4()),
                crate::persons::Role::Client,
            ),
        ),
    )
    .await
    .unwrap();
    mirror::from_source(mirror::during_initial_copy(async {
        assert_eq!(
            family::import_mode(pg, "person").await.unwrap(),
            ImportMode::Refuse
        );
        assert!(matches!(
            mirror::mirror(&source, pg, &MirrorKey::Person(person.id))
                .await
                .unwrap(),
            Mirrored::Unchanged
        ));
        let parity = mirror::parity_all(&source, pg).await.unwrap();
        assert!(!parity.is_empty());
        assert!(parity.iter().all(|report| report.report.tables.is_empty()));
        let reconciled = mirror::reconcile_all(&source, pg).await.unwrap();
        assert!(!reconciled.is_empty());
        assert!(reconciled
            .iter()
            .all(|report| report.report.tables.is_empty()));
        assert_eq!(
            parity
                .iter()
                .map(|report| report.mirror)
                .collect::<Vec<_>>(),
            reconciled
                .iter()
                .map(|report| report.mirror)
                .collect::<Vec<_>>()
        );
    }))
    .await;
    assert!(matches!(
        postgres_backfill::start(&source, pg, 10, None).await,
        Err(postgres_backfill::BackfillError::Family(
            family::FamilyError::NativeOwnership
        ))
    ));
    assert!(matches!(
        postgres_backfill::resume(&source, pg, Uuid::new_v4(), None).await,
        Err(postgres_backfill::BackfillError::Family(
            family::FamilyError::NativeOwnership
        ))
    ));
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM person WHERE id=$1")
        .bind(person.id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 1);
    let histories: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_run")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(histories, 0);
    cleanup(options, native.postgres, operation).await;
}

fn future_schema(gated: bool) -> sqlx::migrate::Migrator {
    let mut migrations = embedded_migrator().iter().cloned().collect::<Vec<_>>();
    let sql = if gated {
        "SELECT pg_advisory_xact_lock(617250019); CREATE TABLE native_origin_upgrade_probe(id UUID PRIMARY KEY);"
    } else {
        "CREATE TABLE native_origin_upgrade_probe(id UUID PRIMARY KEY);"
    };
    migrations.push(sqlx::migrate::Migration::new(
        migrations
            .iter()
            .map(|entry| entry.version)
            .max()
            .and_then(|version| version.checked_add(1))
            .expect("bounded future fixture version"),
        "synthetic native upgrade".into(),
        sqlx::migrate::MigrationType::Simple,
        AssertSqlSafe(sql).into_sql_str(),
        false,
    ));
    sqlx::migrate::Migrator::with_migrations(migrations)
}

#[test]
fn compiled_schema_contract_is_isolated_from_owned_values_and_future_runners() {
    let expected = schema_contract_for(&embedded_migrator());
    assert_eq!(schema_contract(), expected);
    let mut owned = schema_contract();
    owned
        .as_array_mut()
        .expect("migration contract array")
        .clear();
    assert_ne!(owned, expected);
    assert_eq!(schema_contract(), expected);
    let future = schema_contract_for(&future_schema(false));
    assert_ne!(future, expected);
    assert_eq!(schema_contract(), expected);
}

async fn wait_for(pg: &PostgresDb, sql: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let ready: bool = sqlx::query_scalar(AssertSqlSafe(sql))
                .fetch_one(pg.pool())
                .await
                .unwrap();
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

// A completed upgrade attempt closes its dedicated advisory-lock lease on drop.
// Wait for that backend cleanup before the next intentionally sequential attempt.
async fn wait_upgrade_lease_released(pg: &PostgresDb) {
    wait_for(pg, "SELECT NOT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND classid=1869769063 AND objid=1768846704)").await;
}

async fn assert_completed_upgrade_receipt_and_schema(pg: &PostgresDb) {
    let receipts: (i64, i64) =
        sqlx::query_as("SELECT count(*),count(completed_at) FROM database_origin_upgrade")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(receipts, (1, 1));
    let probe: bool =
        sqlx::query_scalar("SELECT to_regclass('public.native_origin_upgrade_probe') IS NOT NULL")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert!(probe);
}

#[tokio::test]
async fn migration_commit_before_upgrade_completion_recovers_only_the_same_bound_operation() {
    let options = options();
    let creation = Uuid::new_v4();
    let binding = tier();
    let operation = Uuid::new_v4();
    let native = create(options.clone(), creation, binding.clone())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let pg = native.postgres();
    let mut migration_gate = pg.pool().acquire().await.unwrap();
    sqlx::query("SELECT pg_advisory_lock(617250019)")
        .execute(&mut *migration_gate)
        .await
        .unwrap();
    let owned_pg = pg.clone();
    let owned_tier = binding.clone();
    let task = tokio::spawn(async move {
        upgrade::upgrade_with(
            &owned_pg,
            creation,
            &owned_tier,
            operation,
            &future_schema(true),
        )
        .await
    });
    wait_for(
        pg,
        "SELECT EXISTS(SELECT 1 FROM database_origin_upgrade WHERE completed_at IS NULL)",
    )
    .await;
    assert!(matches!(admits(pg).await, Err(OriginError::Upgrade)));
    let mut completion_gate = pg.pool().begin().await.unwrap();
    sqlx::query("SELECT upgrade_operation FROM database_origin_upgrade WHERE upgrade_operation=$1 FOR UPDATE")
        .bind(operation).fetch_one(&mut *completion_gate).await.unwrap();
    sqlx::query("SELECT pg_advisory_unlock(617250019)")
        .execute(&mut *migration_gate)
        .await
        .unwrap();
    wait_for(
        pg,
        &format!(
            "SELECT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version={} AND success)",
            future_schema(true)
                .iter()
                .map(|entry| entry.version)
                .max()
                .unwrap()
        ),
    )
    .await;
    let completed: bool = sqlx::query_scalar(
        "SELECT completed_at IS NOT NULL FROM database_origin_upgrade WHERE upgrade_operation=$1",
    )
    .bind(operation)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert!(
        !completed,
        "real migration committed before the blocked completion record"
    );
    assert!(matches!(
        admits_for(pg, &schema_contract_for(&future_schema(true))).await,
        Err(OriginError::Upgrade)
    ));
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    completion_gate.rollback().await.unwrap();
    drop(migration_gate);
    // Cancellation closes the dedicated lease instead of returning a locked
    // session to the pool. Observe that resource cleanup before resuming.
    wait_upgrade_lease_released(pg).await;
    assert!(matches!(
        upgrade::upgrade_with(pg, creation, &binding, Uuid::new_v4(), &future_schema(true)).await,
        Err(OriginError::Upgrade)
    ));
    wait_upgrade_lease_released(pg).await;
    assert!(
        matches!(
            upgrade::upgrade_with(pg, creation, &binding, operation, &future_schema(false)).await,
            Err(OriginError::Upgrade)
        ),
        "same version with a different checksum cannot resume the intent"
    );
    wait_upgrade_lease_released(pg).await;
    upgrade::upgrade_with(pg, creation, &binding, operation, &future_schema(true))
        .await
        .unwrap();
    assert!(admits_for(pg, &schema_contract_for(&future_schema(true)))
        .await
        .unwrap());
    wait_upgrade_lease_released(pg).await;
    upgrade::upgrade_with(pg, creation, &binding, operation, &future_schema(true))
        .await
        .unwrap();
    assert_completed_upgrade_receipt_and_schema(pg).await;
    cleanup(options, native.postgres, creation).await;
}

#[tokio::test]
async fn unregistered_advanced_schema_is_never_adopted_as_an_upgrade() {
    let options = options();
    let creation = Uuid::new_v4();
    let binding = tier();
    let native = create(options.clone(), creation, binding.clone())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    native
        .postgres()
        .migrate(&future_schema(false), None)
        .await
        .unwrap();
    assert!(matches!(
        upgrade::upgrade_with(
            native.postgres(),
            creation,
            &binding,
            Uuid::new_v4(),
            &future_schema(false)
        )
        .await,
        Err(OriginError::Authority)
    ));
    let intents: i64 = sqlx::query_scalar("SELECT count(*) FROM database_origin_upgrade")
        .fetch_one(native.postgres().pool())
        .await
        .unwrap();
    assert_eq!(
        intents, 0,
        "DDL without a prior bound intent cannot acquire an origin revision"
    );
    let probe: bool =
        sqlx::query_scalar("SELECT to_regclass('public.native_origin_upgrade_probe') IS NOT NULL")
            .fetch_one(native.postgres().pool())
            .await
            .unwrap();
    assert!(
        probe,
        "refusal preserves the advanced schema for explicit recovery"
    );
    cleanup(options, native.postgres, creation).await;
}

fn full_native_lookup() -> std::collections::BTreeMap<&'static str, String> {
    let mut values = crate::workflow_engine::WORKFLOWS
        .iter()
        .map(|selection| (selection.flag, "true".into()))
        .collect::<std::collections::BTreeMap<_, _>>();
    values.insert(crate::workflow_engine::COHOST_ENV, "true".into());
    values.insert(crate::workflow_engine::ENGINE_ENV, "dbos".into());
    values.insert(crate::workflow_engine::DBOS_ENABLED_ENV, "true".into());
    values
}

#[tokio::test]
async fn fresh_origin_and_object_epoch_commit_atomically_and_boot_requires_every_native_selector() {
    let options = options();
    let operation = Uuid::new_v4();
    let native = create(options.clone(), operation, tier())
        .await
        .unwrap()
        .initialize()
        .await
        .unwrap();
    let values = full_native_lookup();
    let selection =
        crate::durable_object_handover::require_runtime_selection(native.postgres(), |key| {
            values.get(key).cloned()
        })
        .await
        .unwrap();
    assert!(!selection.legacy_objects());
    for workflow in crate::workflow_engine::WORKFLOWS {
        let mut mixed = values.clone();
        mixed.insert(workflow.flag, "false".into());
        assert!(
            crate::durable_object_handover::require_runtime_selection(native.postgres(), |key| {
                mixed.get(key).cloned()
            })
            .await
            .is_err(),
            "{} must select native execution",
            workflow.name
        );
    }
    for workflow in crate::workflow_engine::DISABLED_BILLING {
        let mut disabled = values.clone();
        disabled.insert(workflow.flag, "false".into());
        assert!(
            !crate::durable_object_handover::require_runtime_selection(native.postgres(), |key| {
                disabled.get(key).cloned()
            })
            .await
            .unwrap()
            .legacy_objects(),
            "{} must not require a billing executor",
            workflow.name
        );
    }
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM database_origin),(SELECT count(*) FROM durable_object_ownership)").fetch_one(native.postgres().pool()).await.unwrap();
    assert_eq!(counts, (1, 1));
    sqlx::query("UPDATE durable_object_handover SET origin='captured_source',source_environment='synthetic-foreign',capture_sha256=repeat('a',64) WHERE capture_id=(SELECT capture_id FROM durable_object_ownership)")
        .execute(native.postgres().pool()).await.unwrap();
    assert!(
        crate::durable_object_handover::require_runtime_selection(native.postgres(), |key| values
            .get(key)
            .cloned())
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM durable_object_ownership")
        .execute(native.postgres().pool())
        .await
        .unwrap();
    assert!(
        crate::durable_object_handover::require_runtime_selection(native.postgres(), |key| values
            .get(key)
            .cloned())
        .await
        .is_err()
    );
    cleanup(options.clone(), native.postgres, operation).await;

    let failed_operation = Uuid::new_v4();
    let binding = tier();
    let created = create(options.clone(), failed_operation, binding.clone())
        .await
        .unwrap();
    created.postgres.migrate_embedded(None).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION synthetic_epoch_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic epoch failure'; END $$; CREATE TRIGGER synthetic_epoch_failure BEFORE INSERT ON durable_object_ownership FOR EACH ROW EXECUTE FUNCTION synthetic_epoch_failure();")
        .execute(created.postgres.pool()).await.unwrap();
    let observer = created.postgres.clone();
    assert!(matches!(
        created.initialize().await,
        Err(OriginError::Storage)
    ));
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM database_origin),(SELECT count(*) FROM durable_object_ownership)").fetch_one(observer.pool()).await.unwrap();
    assert_eq!(
        counts,
        (0, 0),
        "failed object initialization must not leave admitted application origin"
    );
    assert!(matches!(
        resume(options.clone(), failed_operation, binding).await,
        Err(OriginError::Authority)
    ));
    cleanup(options, observer, failed_operation).await;
}

#[tokio::test]
async fn fresh_fixture_requires_full_native_profile_and_invalid_or_unavailable_epochs_refuse() {
    let options = options();
    let inactive = crate::test_support::PostgresFixture::create_capture_target(options.clone())
        .await
        .unwrap();
    assert!(
        crate::durable_object_handover::require_runtime_selection(inactive.postgres(), |_| None)
            .await
            .unwrap()
            .legacy_objects()
    );
    inactive.close().await.unwrap();
    let active = crate::test_support::PostgresFixture::create(options)
        .await
        .unwrap();
    let lookup = |key: &str| match key {
        "NAVIGATOR_DBOS_ENABLED"
        | "NAVIGATOR_DBOS_NOTATION_ENABLED"
        | "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED" => Some("true".into()),
        "NAVIGATOR_WORKFLOW_ENGINE" => Some("dbos".into()),
        _ => None,
    };
    assert!(matches!(
        crate::durable_object_handover::require_runtime_selection(active.postgres(), lookup).await,
        Err(crate::durable_object_handover::HandoverError::NativeProfile)
    ));
    let values = full_native_lookup();
    let lookup = |key: &str| values.get(key).cloned();
    assert!(
        !crate::durable_object_handover::require_runtime_selection(active.postgres(), lookup)
            .await
            .unwrap()
            .legacy_objects()
    );
    assert!(
        crate::durable_object_handover::require_runtime_selection(active.postgres(), |key| {
            if key == "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED" {
                Some("false".into())
            } else {
                lookup(key)
            }
        })
        .await
        .is_err()
    );
    for (key, value) in [
        ("NAVIGATOR_WORKFLOW_ENGINE", None),
        ("NAVIGATOR_WORKFLOW_ENGINE", Some("restate")),
        ("NAVIGATOR_WORKFLOW_ENGINE", Some("invalid")),
        ("NAVIGATOR_DBOS_ENABLED", None),
        ("NAVIGATOR_DBOS_ENABLED", Some("false")),
        ("NAVIGATOR_DBOS_ENABLED", Some("invalid")),
    ] {
        assert!(crate::durable_object_handover::require_runtime_selection(
            active.postgres(),
            |candidate| if candidate == key {
                value.map(str::to_owned)
            } else {
                lookup(candidate)
            }
        )
        .await
        .is_err());
    }
    sqlx::query("UPDATE durable_object_ownership SET ownership_epoch=$1")
        .bind(Uuid::nil())
        .execute(active.postgres().pool())
        .await
        .unwrap();
    assert!(
        crate::durable_object_handover::require_runtime_selection(active.postgres(), lookup)
            .await
            .is_err()
    );
    active.postgres().pool().close().await;
    assert!(
        crate::durable_object_handover::require_runtime_selection(active.postgres(), lookup)
            .await
            .is_err()
    );
    active.close().await.unwrap();
}

#[tokio::test]
async fn existing_esign_ledger_accepts_earlier_handover_migrations_without_native_origin() {
    let options = options();
    let operation = Uuid::new_v4();
    let name = database_name(operation).unwrap();
    let admin = PostgresDb::connect(options.clone()).await.unwrap();
    sqlx::query(AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
        .execute(admin.pool())
        .await
        .unwrap();
    admin.pool().close().await;
    let pg = PostgresDb::connect(options.clone().database(&name))
        .await
        .unwrap();
    let held_back = [
        20_261_119_030_000_i64,
        20_261_119_040_000,
        20_261_119_050_000,
        20_261_121_000_000,
        20_261_126_230_000,
    ];
    let existing = sqlx::migrate::Migrator::with_migrations(
        embedded_migrator()
            .iter()
            .filter(|migration| !held_back.contains(&migration.version))
            .cloned()
            .collect(),
    );
    assert!(
        existing.version_exists(20_261_119_180_000),
        "run after the final migration union"
    );
    existing.run(pg.pool()).await.unwrap();
    let checksum: Vec<u8> = sqlx::query_scalar(
        "SELECT checksum FROM _sqlx_migrations WHERE version=20261119180000 AND success",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert!(matches!(admits(&pg).await, Err(OriginError::Recovery)));
    pg.migrate_embedded(None).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let applied = pg.applied_migrations().await.unwrap();
    for version in held_back {
        assert!(applied.contains(&version));
    }
    let preserved: Vec<u8> = sqlx::query_scalar(
        "SELECT checksum FROM _sqlx_migrations WHERE version=20261119180000 AND success",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(preserved, checksum);
    for table in [
        "durable_object_handover",
        "durable_object_ownership",
        "project_slack_handover",
        "project_slack_delivery",
        "project_slack_delivery_reconciliation",
        "database_origin",
        "database_origin_upgrade",
        "database_recovery",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("public.{table}"))
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert!(exists, "{table} must be created by the real migrations");
    }
    let origins: i64 = sqlx::query_scalar("SELECT count(*) FROM database_origin")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        origins, 0,
        "schema migration never attests fresh-native creation"
    );
    assert!(!admits(&pg).await.unwrap());
    crate::postgres_backfill::family::require_capture_target(&pg)
        .await
        .unwrap();
    assert!(matches!(
        pg.require_application_history().await,
        Err(crate::postgres::PostgresError::History)
    ));
    let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_run")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(receipts, 0, "no captured-source receipts were manufactured");
    cleanup(options, pg, operation).await;
}

/// Recovery never turns copied source identity into target creation authority.
#[tokio::test]
async fn recovery_target_is_bound_pending_and_has_no_fresh_ownership_epoch() {
    use futures::FutureExt as _;
    let options = options();
    let creation = Uuid::new_v4();
    let binding = tier();
    let manifest = b"synthetic unverified manifest bytes".to_vec();
    let source = b"synthetic unverified source identity bytes".to_vec();
    let recovery_operation = Uuid::new_v4();
    let created = create(options.clone(), creation, binding.clone())
        .await
        .unwrap();
    let pending = created
        .initialize_recovery(
            recovery::RecoveryInput::new(recovery_operation, manifest.clone(), source.clone())
                .unwrap(),
        )
        .await
        .unwrap();
    let pg = pending.postgres().clone();
    let result = std::panic::AssertUnwindSafe(async {
        assert_eq!(pending.creation_operation(), creation);
        let origin: (Uuid, String, i64) = sqlx::query_as("SELECT creation_operation,database_name,database_oid FROM database_origin WHERE singleton")
            .fetch_one(pg.pool()).await.unwrap();
        let actual = identity(&pg).await.unwrap();
        assert_eq!(origin, (creation, actual.0, actual.1));
        let captured: i64 = sqlx::query_scalar("SELECT count(*) FROM mirror_backfill_run")
            .fetch_one(pg.pool()).await.unwrap();
        assert_eq!(captured, 0);
        let ownership: i64 = sqlx::query_scalar("SELECT count(*) FROM durable_object_ownership")
            .fetch_one(pg.pool()).await.unwrap();
        assert_eq!(ownership, 0, "restore must supply genuine checkpoint ownership, not a new epoch");
        let retained: (Uuid, Uuid, Vec<u8>, Vec<u8>, String) = sqlx::query_as("SELECT recovery_operation,creation_operation,manifest_bytes,source_identity_bytes,manifest_sha256 FROM database_recovery")
            .fetch_one(pg.pool()).await.unwrap();
        assert_eq!(retained, (recovery_operation, creation, manifest.clone(), source.clone(), hex::encode(sha2::Sha256::digest(&manifest))));
        sqlx::query("ALTER TABLE public.database_recovery RENAME TO missing_recovery_fence").execute(pg.pool()).await.unwrap();
        assert!(matches!(admits(&pg).await, Err(OriginError::Recovery)), "current real migration evidence requires the recovery fence");
        sqlx::query("ALTER TABLE public.database_origin RENAME TO missing_origin_too").execute(pg.pool()).await.unwrap();
        assert!(matches!(admits(&pg).await,Err(OriginError::Recovery)),"missing both tables cannot hide the actual current migration ledger");
        sqlx::query("ALTER TABLE public.missing_origin_too RENAME TO database_origin").execute(pg.pool()).await.unwrap();
        sqlx::query("CREATE TABLE public.database_recovery (LIKE public.missing_recovery_fence INCLUDING ALL)").execute(pg.pool()).await.unwrap();
        assert!(admits(&pg).await.is_err(),"an empty replacement fence never establishes absence of the bound recovery");
        sqlx::query("DROP TABLE public.database_recovery").execute(pg.pool()).await.unwrap();
        sqlx::query("ALTER TABLE public.missing_recovery_fence RENAME TO database_recovery").execute(pg.pool()).await.unwrap();
        let original_function:String=sqlx::query_scalar("SELECT pg_get_functiondef('public.database_recovery_pending()'::regprocedure)").fetch_one(pg.pool()).await.unwrap();
        sqlx::raw_sql("CREATE OR REPLACE FUNCTION public.database_recovery_pending() RETURNS boolean LANGUAGE sql STABLE SECURITY DEFINER SET search_path=pg_catalog AS $$ SELECT false $$").execute(pg.pool()).await.unwrap();
        assert!(matches!(admits(&pg).await,Err(OriginError::Recovery)),"replacement predicate is not trusted as absence proof");
        sqlx::raw_sql(sqlx::AssertSqlSafe(original_function)).execute(pg.pool()).await.unwrap();
        assert!(matches!(admits(&pg).await, Err(OriginError::Recovery)));
        assert!(pg.require_application_history().await.is_err());
        assert!(crate::postgres_backfill::family::require_scope(&pg, crate::postgres_backfill::family::Family::Satellites).await.is_err());
        for sql in ["DELETE FROM database_recovery", "UPDATE database_recovery SET manifest_bytes='changed'::bytea", "TRUNCATE database_recovery"] {
            assert!(sqlx::raw_sql(sql).execute(pg.pool()).await.is_err());
        }
        assert!(matches!(admits(&pg).await, Err(OriginError::Recovery)));
        assert!(resume(options.clone(), creation, binding).await.is_err());
    }).catch_unwind().await;
    drop(pending);
    cleanup(options, pg, creation).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn recovery_input_rejects_nil_empty_and_unbounded_input() {
    use recovery::RecoveryInput;
    assert!(RecoveryInput::new(Uuid::nil(), vec![1], vec![1]).is_err());
    assert!(RecoveryInput::new(Uuid::new_v4(), vec![], vec![1]).is_err());
    assert!(RecoveryInput::new(Uuid::new_v4(), vec![1], vec![]).is_err());
    assert!(RecoveryInput::new(Uuid::new_v4(), vec![1], vec![0; 1024 * 1024 + 1]).is_err());
}

#[tokio::test]
async fn genuine_pre_recovery_origin_upgrades_without_missing_fence_bypass() {
    use futures::FutureExt as _;
    let options = options();
    let creation = Uuid::new_v4();
    let binding = tier();
    let old = embedded_migrator()
        .iter()
        .filter(|m| m.version < 20_261_121_000_000)
        .map(|m| m.version)
        .max()
        .unwrap();
    let native = create(options.clone(), creation, binding.clone())
        .await
        .unwrap()
        .initialize_through(Some(old), None)
        .await
        .unwrap();
    let pg = native.postgres().clone();
    let result = std::panic::AssertUnwindSafe(async {
        let absent: bool =
            sqlx::query_scalar("SELECT to_regclass('public.database_recovery') IS NULL")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert!(absent);
        recovery::require_no_pending(&pg).await.unwrap();
        assert!(matches!(admits(&pg).await, Err(OriginError::Authority)));
        let upgraded = upgrade(options.clone(), creation, binding, Uuid::new_v4())
            .await
            .unwrap();
        assert!(admits(upgraded.postgres()).await.unwrap());
        let exists: bool =
            sqlx::query_scalar("SELECT to_regclass('public.database_recovery') IS NOT NULL")
                .fetch_one(upgraded.postgres().pool())
                .await
                .unwrap();
        assert!(exists);
        upgraded.postgres().pool().close().await;
    })
    .catch_unwind()
    .await;
    drop(native);
    cleanup(options, pg, creation).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn native_upgrade_preserves_a_staged_certificate_without_inventing_a_case_number() {
    use futures::FutureExt as _;

    let options = options();
    let creation = Uuid::new_v4();
    let binding = tier();
    let prior_version = 20_261_122_000_300;
    let native = create(options.clone(), creation, binding.clone())
        .await
        .unwrap()
        .initialize_through(Some(prior_version), None)
        .await
        .unwrap();
    let pg = native.postgres().clone();
    let result = std::panic::AssertUnwindSafe(async {
        let prior_schema = schema_contract();
        let prior_schema = serde_json::Value::Array(
            prior_schema
                .as_array()
                .unwrap()
                .iter()
                .take_while(|migration| migration[0].as_i64().unwrap() <= prior_version)
                .cloned()
                .collect(),
        );
        assert!(admits_for(&pg, &prior_schema).await.unwrap());
        assert!(!sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema = 'public' \
             AND table_name = 'plan_intake' AND column_name = 'plan_case_number')",
        )
        .fetch_one(pg.pool())
        .await
        .unwrap());

        let notice = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO plan_intake (id, firm_id, conversation_id, source_message_id, provider, \
             plan_member_id, plan_number, plan_matter_type, assigned_at, plan_portal_only, \
             conflict_summary, conflict_blocking, conflict_checked, status) VALUES \
             ($1, gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'legal_ease', \
              'SYN-MEMBER', 'SYN-CERTIFICATE', '', now(), false, '', false, false, 'staged')",
        )
        .bind(notice)
        .execute(pg.pool())
        .await
        .unwrap();

        let upgraded = upgrade(options.clone(), creation, binding, Uuid::new_v4())
            .await
            .unwrap();
        assert!(admits(upgraded.postgres()).await.unwrap());
        let row = sqlx::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT status, plan_member_id, plan_number, plan_case_number FROM plan_intake WHERE id = $1",
        )
        .bind(notice)
        .fetch_one(upgraded.postgres().pool())
        .await
        .unwrap();
        assert_eq!(
            row,
            ("staged".into(), "SYN-MEMBER".into(), "SYN-CERTIFICATE".into(), None),
        );
        upgraded.postgres().pool().close().await;
    })
    .catch_unwind()
    .await;
    drop(native);
    cleanup(options, pg, creation).await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// An acknowledged name can be reused by another acknowledged creation;
/// the stale capability must compare its retained OID before administrative ALTER.
#[tokio::test]
async fn recovery_stale_creation_oid_refuses_before_sealing_same_name_replacement() {
    use futures::FutureExt as _;
    let options = options();
    let creation = Uuid::new_v4();
    let recovery_operation = Uuid::new_v4();
    let name = database_name(creation).unwrap();
    let admin = PostgresDb::connect(options.clone()).await.unwrap();
    let mut acknowledged = false;
    let mut first_pool = None;
    let mut replacement_pool = None;
    let mut stale = None;
    let result=std::panic::AssertUnwindSafe(async {
        let created=create(options.clone(),creation,tier()).await.unwrap();
        acknowledged=true;
        first_pool=Some(created.postgres.clone());
        let pending=created.initialize_recovery(recovery::RecoveryInput::new(
            recovery_operation,b"synthetic unverified first manifest".to_vec(),b"synthetic first identity".to_vec(),
        ).unwrap()).await.unwrap();
        let first=pending.postgres().clone();
        let (_,first_oid,first_marker)=identity(&first).await.unwrap();
        let first_marker=first_marker.unwrap();
        assert_eq!(first_marker,recovery::marker(creation,recovery_operation));
        stale=Some(pending);
        // Default min_connections is zero. Drain every positively owned session,
        // keeping the OLD POOL OPEN so verification can reconnect to A2. Closing
        // the pool here would only test PoolClosed, never the retained OID guard.
        while first.pool().size()>0 {
            first.pool().acquire().await.unwrap().close().await.unwrap();
        }
        assert!(!first.pool().is_closed());
        let sessions:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datid=(SELECT oid FROM pg_database WHERE datname=$1)")
            .bind(&name).fetch_one(admin.pool()).await.unwrap();
        assert_eq!(sessions,0,"only our drained sessions may have used A1");
        sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\""))).execute(admin.pool()).await.unwrap();
        acknowledged=false;
        let created=create(options.clone(),creation,tier()).await.unwrap();
        acknowledged=true;
        replacement_pool=Some(created.postgres.clone());
        let replacement=created.initialize_recovery(recovery::RecoveryInput::new(
            Uuid::new_v4(),b"synthetic unverified replacement manifest".to_vec(),b"synthetic replacement identity".to_vec(),
        ).unwrap()).await.unwrap();
        let replacement_oid=identity(replacement.postgres()).await.unwrap().1;
        assert_ne!(first_oid,replacement_oid);
        let origin:serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o").fetch_one(replacement.postgres().pool()).await.unwrap();
        // Both schemas came from real migrations. Copy ONLY the old resource
        // comment, not Origin/ledger rows, so name+marker alone cannot decide.
        sqlx::query(AssertSqlSafe(format!("COMMENT ON DATABASE \"{name}\" IS '{}'",first_marker.replace('\'', "''"))))
            .execute(admin.pool()).await.unwrap();
        let before:bool=sqlx::query_scalar("SELECT datallowconn FROM pg_database WHERE datname=$1").bind(&name).fetch_one(admin.pool()).await.unwrap();
        assert!(before);
        assert!(!first.pool().is_closed());
        let outcome=stale.take().unwrap().verify_and_complete().await;
        assert!(matches!(outcome,Err(OriginError::Recovery)));
        assert!(first.pool().is_closed(),"actual fixed-predicate reads and pool drain must run before the OID refusal");
        let after:(i64,bool)=sqlx::query_as("SELECT oid::bigint,datallowconn FROM pg_database WHERE datname=$1").bind(&name).fetch_one(admin.pool()).await.unwrap();
        assert_eq!(after,(replacement_oid,true),"a stale capability must not seal the positively owned replacement");
        let preserved:serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(o) FROM public.database_origin o").fetch_one(replacement.postgres().pool()).await.unwrap();
        assert_eq!(preserved,origin);
        let value:i32=sqlx::query_scalar("SELECT 1").fetch_one(replacement.postgres().pool()).await.unwrap();
        assert_eq!(value,1);
    }).catch_unwind().await;
    // No FORCE, backend termination, shared roles or unacknowledged DROP.
    if let Some(pg) = first_pool {
        pg.pool().close().await;
    }
    if let Some(pg) = replacement_pool {
        pg.pool().close().await;
    }
    drop(stale);
    let cleanup = if acknowledged {
        sqlx::query(AssertSqlSafe(format!("DROP DATABASE \"{name}\"")))
            .execute(admin.pool())
            .await
            .map(|_| ())
    } else {
        Ok(())
    };
    admin.pool().close().await;
    if let Err(panic) = result {
        if cleanup.is_err() {
            eprintln!("owned replacement cleanup also failed");
        }
        std::panic::resume_unwind(panic);
    }
    cleanup.unwrap();
}
