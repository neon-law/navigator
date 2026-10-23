//! Synthetic multi-database coverage and Iceberg-reader recovery drill.
use archives::coverage::{export_pair, inventory, CoverageManifest, DatabaseSource, Rule};
use cloud::{FsStorage, ObjectListing, StorageError, StorageService, StoredObject};
use futures::TryStreamExt;
use sqlx::{postgres::PgConnectOptions, PgPool};

use arrow_array58::{
    Array, BinaryArray, BooleanArray, Int32Array, Int64Array, RecordBatch, StringArray,
};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

const NAV_SCHEMA: &str = "CREATE TABLE public.probe (id bigint PRIMARY KEY, status text); CREATE TABLE public.empty (id bigint); CREATE VIEW public.derived AS SELECT * FROM public.probe";
const FORGE_SCHEMA: &str = "CREATE SCHEMA forge; CREATE TABLE forge.repository (id bigint PRIMARY KEY, name text, private boolean, payload bytea, metadata jsonb)";
const GRAPH_SCHEMA: &str = "CREATE EXTENSION age; LOAD 'age'; SET search_path = ag_catalog, public; SELECT create_graph('coverage_graph'); SELECT create_vlabel('coverage_graph', 'Probe'); SELECT create_elabel('coverage_graph', 'Link')";

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
            if relation.schema == "public"
                && relation.name == "derived"
                && relation.kind == archives::coverage::RelationKind::View
            {
                Rule::Derived {
                    relation,
                    reason: "rebuild from probe".into(),
                }
            } else {
                Rule::Export(relation)
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
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL port required"
        );
        return;
    };
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
            .port(port.parse().unwrap())
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
    let storage = FsStorage::new(dir.path()).await.unwrap();
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

    // Load only objects from cloud::StorageService into the reader's FileIO.
    let io = iceberg::io::FileIO::new_with_memory();
    for object in storage.list("").await.unwrap() {
        let bytes = storage.get(&object.key).await.unwrap().bytes;
        io.new_output(format!("{base}/{}", object.key))
            .unwrap()
            .write(bytes.into())
            .await
            .unwrap();
    }
    for (db, target) in paired.databases.iter().zip([&target_nav, &target_forge]) {
        let mut restored_largeobjects = std::collections::BTreeSet::new();
        let mut ordered = db
            .tables
            .iter()
            .zip(&db.schemas)
            .zip(&db.iceberg_metadata_keys)
            .collect::<Vec<_>>();
        ordered.sort_by_key(|((_, schema), _)| schema.relation.name != "workflow_status");
        for ((entry, schema), metadata_key) in ordered {
            let object = storage.get(&entry.key).await.unwrap();
            let hash =
                Sha256::digest(&object.bytes)
                    .iter()
                    .fold(String::new(), |mut output, byte| {
                        write!(output, "{byte:02x}").unwrap();
                        output
                    });
            assert_eq!(entry.sha256, hash);
            let table = iceberg::table::StaticTable::from_metadata_file(
                &format!("{base}/{metadata_key}"),
                iceberg::TableIdent::from_strs(["coverage", "probe"]).unwrap(),
                io.clone(),
            )
            .await
            .unwrap();
            let tasks = table.scan().build().unwrap().plan_files().await.unwrap();
            let batches: Vec<_> = table
                .reader_builder()
                .build()
                .read(tasks)
                .unwrap()
                .stream()
                .try_collect()
                .await
                .unwrap();
            assert_eq!(
                batches.iter().map(RecordBatch::num_rows).sum::<usize>(),
                entry.rows
            );
            if schema.relation.schema == "pg_catalog" && schema.relation.name == "pg_largeobject" {
                for batch in batches {
                    for row in 0..batch.num_rows() {
                        let oid = batch
                            .column_by_name("loid")
                            .unwrap()
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .unwrap()
                            .value(row);
                        let page = batch
                            .column_by_name("pageno")
                            .unwrap()
                            .as_any()
                            .downcast_ref::<Int32Array>()
                            .unwrap()
                            .value(row);
                        let data = arrow_cast58::cast(
                            batch
                                .column_by_name("data")
                                .expect("large-object data column"),
                            &arrow_schema58::DataType::Binary,
                        )
                        .unwrap();
                        let bytes = data
                            .as_any()
                            .downcast_ref::<BinaryArray>()
                            .unwrap()
                            .value(row);
                        if restored_largeobjects.insert(oid.to_string()) {
                            sqlx::query("SELECT lo_create($1::text::oid)")
                                .bind(oid)
                                .execute(target)
                                .await
                                .unwrap();
                        }
                        sqlx::query("SELECT lo_put($1::text::oid, $2, $3)")
                            .bind(oid)
                            .bind(i64::from(page) * 2048)
                            .bind(bytes)
                            .execute(target)
                            .await
                            .unwrap();
                    }
                }
                continue;
            }
            if ["pg_catalog", "ag_catalog"].contains(&schema.relation.schema.as_str()) {
                // Native schema and large-object procedures reconstruct
                // these catalogs and resolve identifiers in the target.
                continue;
            }
            if schema.relation.kind == archives::coverage::RelationKind::Sequence {
                for batch in batches {
                    let value = batch
                        .column_by_name("last_value")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<Int64Array>()
                        .unwrap()
                        .value(0);
                    let called = batch
                        .column_by_name("is_called")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<BooleanArray>()
                        .unwrap()
                        .value(0);
                    let qualified = format!(
                        "\"{}\".\"{}\"",
                        schema.relation.schema, schema.relation.name
                    );
                    sqlx::query("SELECT setval($1::regclass, $2, $3)")
                        .bind(qualified)
                        .bind(value)
                        .bind(called)
                        .execute(target)
                        .await
                        .unwrap();
                }
                continue;
            }
            for batch in batches {
                for row in 0..batch.num_rows() {
                    let qualified = format!(
                        "\"{}\".\"{}\"",
                        schema.relation.schema, schema.relation.name
                    );
                    let columns = schema
                        .columns
                        .iter()
                        .map(|(name, _)| format!("\"{name}\""))
                        .collect::<Vec<_>>()
                        .join(",");
                    let placeholders = schema
                        .columns
                        .iter()
                        .enumerate()
                        .map(|(index, (_, ty))| format!("${}::text::{ty}", index + 1))
                        .collect::<Vec<_>>()
                        .join(",");
                    let statement =
                        format!("INSERT INTO {qualified} ({columns}) VALUES ({placeholders})");
                    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement));
                    for (column, (_, pg_type)) in batch.columns().iter().zip(&schema.columns) {
                        let value = (!column.is_null(row)).then(|| {
                            arrow_cast58::display::array_value_to_string(column, row).unwrap()
                        });
                        let value = if pg_type == "bytea" {
                            value.map(|hex| format!("\\x{hex}"))
                        } else {
                            value
                        };
                        query = query.bind(value);
                    }
                    query.execute(target).await.unwrap();
                }
            }
        }
    }
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
    let restored_large_object: Vec<u8> = sqlx::query_scalar("SELECT lo_get($1::bigint::oid)")
        .bind(large_object)
        .fetch_one(&target_nav)
        .await
        .unwrap();
    assert_eq!(restored_large_object, [0_u8, 255].repeat(8192));
    // Compare every stored table, including DBOS and large-object storage.
    // Native AGE schema reconstruction maps source OIDs to target OIDs.
    for (db, source, target) in [
        (&paired.databases[0], &navigator, &target_nav),
        (&paired.databases[1], &forgejo, &target_forge),
    ] {
        for schema in &db.schemas {
            if schema.relation.schema == "ag_catalog" {
                continue;
            }
            let relation = format!(
                "\"{}\".\"{}\"",
                schema.relation.schema, schema.relation.name
            );
            let statement = if schema.relation.kind == archives::coverage::RelationKind::Sequence {
                format!("SELECT md5(last_value::text || ':' || is_called::text), 1::bigint FROM {relation}")
            } else {
                format!("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, ',' ORDER BY row_to_json(t)::text), '')), count(*)::bigint FROM ONLY {relation} t")
            };
            let original: (String, i64) = sqlx::query_as(sqlx::AssertSqlSafe(statement.clone()))
                .fetch_one(source)
                .await
                .unwrap();
            let restored: (String, i64) = sqlx::query_as(sqlx::AssertSqlSafe(statement))
                .fetch_one(target)
                .await
                .unwrap();
            assert_eq!(
                original, restored,
                "complete stored-table restoration for {relation}"
            );
        }
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
    for pool in [&navigator, &forgejo] {
        sqlx::raw_sql("CREATE TABLE public.unclassified (id bigint)")
            .execute(pool)
            .await
            .unwrap();
        let failed_dir = tempfile::tempdir().unwrap();
        let failed_storage = FsStorage::new(failed_dir.path()).await.unwrap();
        assert!(export_pair(
            source("navigator", &navigator, &nav_coverage),
            source("forgejo", &forgejo, &forge_coverage),
            &failed_storage,
            base
        )
        .await
        .is_err());
        assert!(failed_storage.list("").await.unwrap().is_empty());
        sqlx::raw_sql("DROP TABLE public.unclassified")
            .execute(pool)
            .await
            .unwrap();
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
