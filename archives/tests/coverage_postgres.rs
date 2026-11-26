//! Synthetic multi-database coverage and Iceberg-reader recovery drill.
use archives::coverage::{export_pair, inventory, CoverageManifest, DatabaseSource, Rule};
use cloud::{FsStorage, ObjectListing, StorageError, StorageService, StoredObject};
use sqlx::{postgres::PgConnectOptions, PgPool};
use store::postgres::PostgresDb;
use store::postgres_backfill::family::{require_scope, Family, FamilyError, SourceReceipt};

#[path = "coverage_postgres/native_origin.rs"]
mod native_origin;
#[path = "coverage_postgres/native_ownership.rs"]
mod native_ownership;
#[path = "coverage_postgres/restore.rs"]
mod restore;

const NAV_SCHEMA: &str = "CREATE TABLE public.probe (id bigint PRIMARY KEY, status text); CREATE TABLE public.empty (id bigint); CREATE VIEW public.derived AS SELECT * FROM public.probe";
const FORGE_SCHEMA: &str = "CREATE SCHEMA forge; CREATE TABLE forge.repository (id bigint PRIMARY KEY, name text, private boolean, payload bytea, metadata jsonb)";
const GRAPH_SCHEMA: &str = "CREATE EXTENSION age; LOAD 'age'; SET search_path = ag_catalog, public; SELECT create_graph('coverage_graph'); SELECT create_vlabel('coverage_graph', 'Probe'); SELECT create_elabel('coverage_graph', 'Link')";

async fn ledger_schema(pool: &PgPool) {
    for sql in [
        include_str!("../../store/postgres/migrations/20261021160000_create_mirror_backfill.sql"),
        include_str!(
            "../../store/postgres/migrations/20261117200000_scope_mirror_backfill_runs.sql"
        ),
    ] {
        sqlx::raw_sql(sql).execute(pool).await.unwrap();
    }
}

async fn seed_scoped_ledger(pool: &PgPool) {
    let family = Family::Satellites;
    let receipt = SourceReceipt {
        format_version: 1,
        consistency: "operator_quiesced".into(),
        export_bytes: 321,
        export_sha256: "ab".repeat(32),
        schema_sha256: "cd".repeat(32),
        table_counts: family
            .tables()
            .iter()
            .map(|name| ((*name).into(), 2))
            .collect(),
    };
    receipt.validate(family).unwrap();
    let receipt = serde_json::to_value(receipt).unwrap();
    let mut previous = receipt.clone();
    previous["export_sha256"] = serde_json::json!("ef".repeat(32));
    let run = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO mirror_backfill_run (id,batch_size,status,completed_at,scope,source_receipt,source_receipt_history) VALUES ($1,7,'completed',now(),$2,$3,$4)")
        .bind(run).bind(family.scope()).bind(receipt).bind(serde_json::json!([previous]))
        .execute(pool).await.unwrap();
    for table in family.tables() {
        let verification = serde_json::json!({
            "source_rows": 2, "target_rows": 2, "differences": 0,
            "references_checked": true, "foreign_keys_checked": true,
            "references": [], "reset_facts_checked": false, "reset_fact_differences": 0
        });
        sqlx::query("INSERT INTO mirror_backfill_checkpoint (run_id,mirror,batches,applied,completed_at,verification) VALUES ($1,$2,1,2,now(),$3)")
            .bind(run).bind(format!("table:{table}")).bind(verification).execute(pool).await.unwrap();
    }
    sqlx::query("INSERT INTO mirror_backfill_failure (id,run_id,mirror,row_table,row_id,reason) VALUES ($1,$2,'table:attestation','attestation',$3,'synthetic interrupted attempt')")
        .bind(uuid::Uuid::new_v4()).bind(run).bind(uuid::Uuid::new_v4().to_string())
        .execute(pool).await.unwrap();
}

async fn assert_ledger_preserved(source: &PgPool, target: &PgPool) {
    for table in [
        "mirror_backfill_run",
        "mirror_backfill_checkpoint",
        "mirror_backfill_failure",
    ] {
        let statement = format!("SELECT to_jsonb(r) FROM {table} r ORDER BY to_jsonb(r)::text");
        let original: Vec<serde_json::Value> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str()))
                .fetch_all(source)
                .await
                .unwrap();
        let restored: Vec<serde_json::Value> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str()))
                .fetch_all(target)
                .await
                .unwrap();
        assert!(
            !original.is_empty(),
            "the recovery fixture must exercise {table}"
        );
        assert_eq!(
            original, restored,
            "all scoped proof fields survive {table} recovery"
        );
    }
}

async fn database(admin: &PgPool, name: &str) -> PgPool {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(admin)
        .await
        .unwrap();
    PgPool::connect_with(admin.connect_options().as_ref().clone().database(name))
        .await
        .unwrap()
}

async fn manifest(pool: &PgPool) -> CoverageManifest {
    let mut tx = pool.begin().await.unwrap();
    let relations = inventory(&mut tx)
        .await
        .unwrap()
        .into_iter()
        .map(|relation| {
            let reason = match (relation.schema.as_str(), relation.name.as_str()) {
                ("public", "derived") => "rebuild from probe",
                ("public", "person_directory" | "firm_visible_person" | "project_list") => {
                    "recreated by the canonical Navigator schema"
                }
                ("public", "geography_columns" | "geometry_columns") => "recreated by PostGIS",
                _ => return Rule::Export(relation),
            };
            Rule::Derived {
                relation,
                reason: reason.into(),
            }
        })
        .collect();
    tx.rollback().await.unwrap();
    CoverageManifest { relations }
}

async fn apply_fixture_dump(pool: &PgPool, key: &str) {
    if let Ok(path) = std::env::var(key) {
        let sql = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("\\restrict ") && !line.starts_with("\\unrestrict "))
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .unwrap();
    }
}

fn source<'a>(
    name: &'a str,
    pool: &'a PgPool,
    coverage: &'a CoverageManifest,
) -> DatabaseSource<'a> {
    DatabaseSource {
        name,
        pool,
        coverage,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated multi-source recovery drill follows extraction, Iceberg reading, restoration, and failure injection"
)]
async fn postgres_paired_sources_fail_closed_and_rehydrate_from_iceberg() {
    let port = store::test_support::postgres_test_port();
    let dump_settings = [
        "NAVIGATOR_TEST_FORGEJO_PRE_SCHEMA",
        "NAVIGATOR_TEST_FORGEJO_DATA",
        "NAVIGATOR_TEST_FORGEJO_POST_SCHEMA",
    ];
    let dump_count = dump_settings
        .iter()
        .filter(|key| std::env::var(key).is_ok())
        .count();
    assert!(
        dump_count == 0 || dump_count == dump_settings.len(),
        "full Forgejo fixture requires all three dump sections"
    );
    let admin = PgPool::connect_with(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database("smoke"),
    )
    .await
    .unwrap();
    let id = uuid::Uuid::new_v4().simple().to_string();
    let navigator = database(&admin, &format!("coverage_nav_{id}")).await;
    let forgejo = database(&admin, &format!("coverage_forge_{id}")).await;
    let target_nav = database(&admin, &format!("coverage_restore_nav_{id}")).await;
    let target_forge = database(&admin, &format!("coverage_restore_forge_{id}")).await;
    // Optional private dumps come only from this task's quiesced synthetic
    // Forgejo fixture. They exercise its full owned schema without checking
    // generated service state or credentials into the repository.
    for pool in [&forgejo, &target_forge] {
        apply_fixture_dump(pool, "NAVIGATOR_TEST_FORGEJO_PRE_SCHEMA").await;
    }
    apply_fixture_dump(&forgejo, "NAVIGATOR_TEST_FORGEJO_DATA").await;
    apply_fixture_dump(&forgejo, "NAVIGATOR_TEST_FORGEJO_POST_SCHEMA").await;
    if dump_count > 0 {
        let tables: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
                .fetch_one(&forgejo)
                .await
                .unwrap();
        assert_eq!(tables, 128, "pinned Forgejo owned schema");
        let repository: i64 = sqlx::query_scalar("INSERT INTO public.repository (owner_id, owner_name, lower_name, name, is_private, topics) SELECT id, name, 'archive-probe', 'archive-probe', true, json_build_array('synthetic') FROM public.\"user\" ORDER BY id LIMIT 1 RETURNING id")
            .fetch_one(&forgejo).await.unwrap();
        assert!(repository > 0);
    }
    // Each source owns its own schema/restore procedure. These DDL fixtures
    // are deliberately different, so a public-only restore cannot pass.
    for pool in [&navigator, &target_nav] {
        sqlx::raw_sql(NAV_SCHEMA).execute(pool).await.unwrap();
        ledger_schema(pool).await;
    }
    seed_scoped_ledger(&navigator).await;
    let source_ledger = PostgresDb::connect(navigator.connect_options().as_ref().clone())
        .await
        .unwrap();
    let target_ledger = PostgresDb::connect(target_nav.connect_options().as_ref().clone())
        .await
        .unwrap();
    assert!(
        matches!(
            require_scope(&source_ledger, Family::Satellites).await,
            Err(FamilyError::NativeOrigin(
                store::postgres::native_origin::OriginError::Recovery
            ))
        ),
        "synthetic receipt rows are not a known application database history"
    );
    assert!(matches!(
        require_scope(&target_ledger, Family::Satellites).await,
        Err(FamilyError::NativeOrigin(
            store::postgres::native_origin::OriginError::Recovery
        ))
    ));
    let postgis_available: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = 'postgis')",
    )
    .fetch_one(&navigator)
    .await
    .unwrap();
    if postgis_available {
        for pool in [&navigator, &target_nav] {
            sqlx::raw_sql(
                "CREATE EXTENSION postgis;
                 CREATE TABLE public.spatial_probe (id bigint PRIMARY KEY, segments geometry(MultiLineString,4326));
                 TRUNCATE public.spatial_ref_sys",
            )
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::raw_sql(
            "INSERT INTO public.spatial_probe VALUES
             (1, ST_GeomFromText('MULTILINESTRING((-110 5,-105 6),(-90 32,-80 36))',4326)),
             (2, NULL)",
        )
        .execute(&navigator)
        .await
        .unwrap();
    }
    for pool in [&forgejo, &target_forge] {
        sqlx::raw_sql(FORGE_SCHEMA).execute(pool).await.unwrap();
    }
    sqlx::raw_sql("INSERT INTO public.probe VALUES (1, 'SUCCESS'), (2, NULL)")
        .execute(&navigator)
        .await
        .unwrap();
    sqlx::raw_sql("INSERT INTO forge.repository VALUES (1, 'synthetic', true, decode(repeat('00ff', 4096), 'hex'), jsonb_build_object('value', 9007199254740993))")
        .execute(&forgejo)
        .await
        .unwrap();
    // The pinned native SDK owns and creates its real system schema.
    for (pool, seed) in [(&navigator, true), (&target_nav, false)] {
        let database: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(pool)
            .await
            .unwrap();
        let url = format!("postgresql://postgres:synthetic@127.0.0.1:{port}/{database}");
        let mut config = dbos::Config::new("archive-coverage", &url);
        config.schema = "dbos".into();
        config.app_version = Some("synthetic-v1".into());
        let runtime = dbos::DBOS::new(config);
        let workflow = runtime
            .register_workflow("archive-probe", |()| async {
                dbos::step("checkpoint", || async { Ok::<_, dbos::Error>(7_u32) }).await
            })
            .unwrap();
        runtime.launch().await.unwrap();
        if seed {
            assert_eq!(
                workflow
                    .run_with(
                        (),
                        dbos::RunOptions {
                            workflow_id: Some("archive-synthetic"),
                            ..Default::default()
                        }
                    )
                    .await
                    .unwrap(),
                7
            );
        }
        runtime.shutdown().await;
    }
    // Clear SDK-created target rows before replaying the archived state.
    let dbos_tables: Vec<String> =
        sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname = 'dbos'")
            .fetch_all(&target_nav)
            .await
            .unwrap();
    let truncate = dbos_tables
        .iter()
        .map(|name| format!("dbos.\"{name}\""))
        .collect::<Vec<_>>()
        .join(",");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("TRUNCATE {truncate} CASCADE")))
        .execute(&target_nav)
        .await
        .unwrap();
    for pool in [&navigator, &target_nav] {
        sqlx::raw_sql(GRAPH_SCHEMA).execute(pool).await.unwrap();
    }
    sqlx::raw_sql("LOAD 'age'; SET search_path = ag_catalog, public; SELECT * FROM cypher('coverage_graph', $$ CREATE (:Probe {value: 'synthetic'})-[:Link {weight: 7}]->(:Probe {value: 'second'}) $$) AS (value agtype)")
        .execute(&navigator).await.unwrap();
    let large_object: i64 =
        sqlx::query_scalar("SELECT lo_from_bytea(0, decode(repeat('00ff', 8192), 'hex'))::bigint")
            .fetch_one(&navigator)
            .await
            .unwrap();
    let nav_coverage = manifest(&navigator).await;
    let forge_coverage = manifest(&forgejo).await;
    // Catalog inventory remains visible when the export principal cannot
    // read the rows. Denied DBOS tables must never disappear from coverage.
    let reader = format!("coverage_reader_{id}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {reader} LOGIN PASSWORD 'synthetic'"
    )))
    .execute(&admin)
    .await
    .unwrap();
    let denied = PgPool::connect_with(
        navigator
            .connect_options()
            .as_ref()
            .clone()
            .username(&reader),
    )
    .await
    .unwrap();
    let mut tx = denied.begin().await.unwrap();
    let denied_inventory = inventory(&mut tx).await.unwrap();
    assert!(denied_inventory
        .iter()
        .any(|relation| relation.schema == "dbos"
            && relation.name == "workflow_status"
            && !relation.accessible));
    tx.rollback().await.unwrap();
    let denied_dir = tempfile::tempdir().unwrap();
    let denied_storage = FsStorage::new(denied_dir.path()).await.unwrap();
    assert!(export_pair(
        source("navigator", &denied, &nav_coverage),
        source("forgejo", &forgejo, &forge_coverage),
        &denied_storage,
        "memory://coverage"
    )
    .await
    .is_err());
    assert!(denied_storage.list("").await.unwrap().is_empty());
    denied.close().await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP ROLE {reader}")))
        .execute(&admin)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage = ZeroDelete {
        inner: FsStorage::new(dir.path()).await.unwrap(),
    };
    let base = "memory://coverage";
    let paired = export_pair(
        source("navigator", &navigator, &nav_coverage),
        source("forgejo", &forgejo, &forge_coverage),
        &storage,
        base,
    )
    .await
    .unwrap();
    assert!(!paired.cross_database_atomic);
    assert_eq!(paired.databases.len(), 2);
    assert_ne!(
        paired.databases[0].database_name,
        paired.databases[1].database_name
    );
    assert!(paired.databases.iter().all(|db| !db.snapshot.is_empty()));

    restore::restore_pair(
        &paired,
        &storage,
        base,
        &target_nav,
        &target_forge,
        &target_ledger,
    )
    .await;
    assert_ledger_preserved(&navigator, &target_nav).await;
    assert!(
        matches!(
            require_scope(&target_ledger, Family::Satellites).await,
            Err(FamilyError::NativeOrigin(
                store::postgres::native_origin::OriginError::Recovery
            ))
        ),
        "row copying never admits this unmigrated synthetic target"
    );
    source_ledger.pool().close().await;
    target_ledger.pool().close().await;
    // Service-owned indexes and foreign keys are installed after its rows,
    // as in PostgreSQL's native pre-data/data/post-data restore procedure.
    apply_fixture_dump(&target_forge, "NAVIGATOR_TEST_FORGEJO_POST_SCHEMA").await;
    for (pool, target, relation) in [
        (&navigator, &target_nav, "public.probe"),
        (&forgejo, &target_forge, "forge.repository"),
        (&navigator, &target_nav, "coverage_graph.\"Probe\""),
        (&navigator, &target_nav, "coverage_graph.\"Link\""),
    ] {
        let statement = format!(
            "SELECT md5(string_agg(row_to_json(t)::text, ',' ORDER BY id)) FROM {relation} t"
        );
        let source: String = sqlx::query_scalar(sqlx::AssertSqlSafe(statement.clone()))
            .fetch_one(pool)
            .await
            .unwrap();
        let restored: String = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_one(target)
            .await
            .unwrap();
        assert_eq!(source, restored);
    }

    for relation in ["dbos.workflow_status", "dbos.operation_outputs"] {
        let statement = format!("SELECT md5(string_agg(row_to_json(t)::text, ',' ORDER BY workflow_uuid)) FROM {relation} t");
        let source: String = sqlx::query_scalar(sqlx::AssertSqlSafe(statement.clone()))
            .fetch_one(&navigator)
            .await
            .unwrap();
        let target: String = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_one(&target_nav)
            .await
            .unwrap();
        assert_eq!(
            source, target,
            "DBOS state and checkpoint rows rehydrate exactly"
        );
    }
    if postgis_available {
        let geometry = "SELECT ST_AsEWKB(segments), ST_SRID(segments), GeometryType(segments)
                        FROM public.spatial_probe ORDER BY id";
        let read_geometry = |pool| {
            sqlx::query_as::<_, (Option<Vec<u8>>, Option<i32>, Option<String>)>(geometry)
                .fetch_all(pool)
        };
        let source = read_geometry(&navigator).await.unwrap();
        let restored = read_geometry(&target_nav).await.unwrap();
        assert_eq!(source, restored);
        assert_eq!(restored[0].1, Some(4326));
        assert_eq!(restored[0].2.as_deref(), Some("MULTILINESTRING"));
        assert_eq!(restored[1], (None, None, None));
    }
    let restored_large_object: Vec<u8> = sqlx::query_scalar("SELECT lo_get($1::bigint::oid)")
        .bind(large_object)
        .fetch_one(&target_nav)
        .await
        .unwrap();
    assert_eq!(restored_large_object, [0_u8, 255].repeat(8192));
    for (db, source, target) in [
        (&paired.databases[0], &navigator, &target_nav),
        (&paired.databases[1], &forgejo, &target_forge),
    ] {
        restore::assert_database_equal(db, source, target).await;
    }
    let source_oid: String = sqlx::query_scalar(
        "SELECT graphid::text FROM ag_catalog.ag_graph WHERE name = 'coverage_graph'",
    )
    .fetch_one(&navigator)
    .await
    .unwrap();
    let target_oid: String = sqlx::query_scalar(
        "SELECT graphid::text FROM ag_catalog.ag_graph WHERE name = 'coverage_graph'",
    )
    .fetch_one(&target_nav)
    .await
    .unwrap();
    assert_ne!(
        source_oid, target_oid,
        "native schema reconstruction resolves target OIDs"
    );
    for pool in [&navigator, &target_nav] {
        let mut connection = pool.acquire().await.unwrap();
        sqlx::raw_sql("LOAD 'age'; SET search_path = ag_catalog, public")
            .execute(&mut *connection)
            .await
            .unwrap();
        let count: String = sqlx::query_scalar("SELECT value::text FROM ag_catalog.cypher('coverage_graph', $$ MATCH (:Probe)-[r:Link]->(:Probe) RETURN count(r) $$) AS (value ag_catalog.agtype)")
            .fetch_one(&mut *connection).await.unwrap();
        assert_eq!(
            count, "1",
            "restored AGE graph is traversable with target catalog OIDs"
        );
    }

    assert_eq!(
        paired.databases[0]
            .tables
            .iter()
            .find(|table| table.table == "public.empty")
            .unwrap()
            .rows,
        0
    );
    for refused in ["postgres/forgejo/", "/metadata/", "postgres/_paired/"] {
        let failed_dir = tempfile::tempdir().unwrap();
        let failed_storage = RefuseWrites {
            inner: FsStorage::new(failed_dir.path()).await.unwrap(),
            refused,
        };
        assert!(export_pair(
            source("navigator", &navigator, &nav_coverage),
            source("forgejo", &forgejo, &forge_coverage),
            &failed_storage,
            base
        )
        .await
        .is_err());
        assert!(failed_storage
            .list("postgres/_paired/")
            .await
            .unwrap()
            .is_empty());
    }

    // A failure to connect to either database leaves no paired publication.
    let unavailable = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(100))
        .connect_lazy_with(admin.connect_options().as_ref().clone().port(1));
    for (nav, forge) in [(&unavailable, &forgejo), (&navigator, &unavailable)] {
        let failure_dir = tempfile::tempdir().unwrap();
        let failure_storage = FsStorage::new(failure_dir.path()).await.unwrap();
        assert!(export_pair(
            source("navigator", nav, &nav_coverage),
            source("forgejo", forge, &forge_coverage),
            &failure_storage,
            base
        )
        .await
        .is_err());
        assert!(failure_storage.list("").await.unwrap().is_empty());
    }

    // Both added and dropped tables must prevent publication, including empty ones.
    for (pool, create, remove, relation) in [
        (
            &navigator,
            "LOAD 'age'; SELECT ag_catalog.create_graph('storage_probe')",
            "LOAD 'age'; SELECT ag_catalog.drop_graph('storage_probe', true)",
            "storage_probe._ag_label_edge",
        ),
        (
            &forgejo,
            "CREATE TABLE public.unclassified (id bigint)",
            "DROP TABLE public.unclassified",
            "public.unclassified",
        ),
    ] {
        sqlx::raw_sql(create).execute(pool).await.unwrap();
        let failed_dir = tempfile::tempdir().unwrap();
        let failed_storage = FsStorage::new(failed_dir.path()).await.unwrap();
        let error = export_pair(
            source("navigator", &navigator, &nav_coverage),
            source("forgejo", &forgejo, &forge_coverage),
            &failed_storage,
            base,
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains(&format!("unclassified relation {relation}")));
        assert!(failed_storage.list("").await.unwrap().is_empty());
        sqlx::raw_sql(remove).execute(pool).await.unwrap();
    }
    sqlx::raw_sql("DROP TABLE public.empty")
        .execute(&navigator)
        .await
        .unwrap();
    let failure_dir = tempfile::tempdir().unwrap();
    let failure_storage = FsStorage::new(failure_dir.path()).await.unwrap();
    assert!(export_pair(
        source("navigator", &navigator, &nav_coverage),
        source("forgejo", &forgejo, &forge_coverage),
        &failure_storage,
        base
    )
    .await
    .is_err());
    assert!(failure_storage.list("").await.unwrap().is_empty());
    for pool in [&navigator, &forgejo, &target_nav, &target_forge] {
        let name: String = sqlx::query_scalar("SELECT current_database()::text")
            .fetch_one(pool)
            .await
            .unwrap();
        pool.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
    }
}

struct RefuseWrites {
    inner: FsStorage,
    refused: &'static str,
}

#[async_trait::async_trait]
impl StorageService for RefuseWrites {
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), StorageError> {
        if key.contains(self.refused) {
            return Err(StorageError::Unsupported("synthetic archive write failure"));
        }
        self.inner.put(key, bytes, content_type).await
    }
    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<cloud::ImmutableObject, StorageError> {
        if key.contains(self.refused) {
            return Err(StorageError::Unsupported("synthetic archive write failure"));
        }
        self.inner.put_immutable(key, bytes, content_type).await
    }
    async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
        self.inner.get(key).await
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.inner.delete(key).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectListing>, StorageError> {
        self.inner.list(prefix).await
    }
    async fn signed_url(
        &self,
        key: &str,
        ttl: std::time::Duration,
    ) -> Result<String, StorageError> {
        self.inner.signed_url(key, ttl).await
    }
}

/// The drill's main storage: forwards everything to a tempdir `FsStorage`
/// except `delete`, which panics — an undisturbed paired publication is
/// the proof that the export path never deletes.
struct ZeroDelete {
    inner: FsStorage,
}

#[async_trait::async_trait]
impl StorageService for ZeroDelete {
    async fn put(&self, key: &str, bytes: &[u8], content_type: &str) -> Result<(), StorageError> {
        self.inner.put(key, bytes, content_type).await
    }
    async fn put_immutable(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<cloud::ImmutableObject, StorageError> {
        self.inner.put_immutable(key, bytes, content_type).await
    }
    async fn get(&self, key: &str) -> Result<StoredObject, StorageError> {
        self.inner.get(key).await
    }
    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        panic!("the paired coverage drill must never delete: {key}");
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectListing>, StorageError> {
        self.inner.list(prefix).await
    }
    async fn signed_url(
        &self,
        key: &str,
        ttl: std::time::Duration,
    ) -> Result<String, StorageError> {
        self.inner.signed_url(key, ttl).await
    }
}
