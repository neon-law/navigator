//! Disposable `SQLx` driver and timestamped migration-runner contract.
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::PgPool;
use uuid::Uuid;

struct Database {
    admin: PgPool,
    pool: PgPool,
    name: String,
    pg: store::postgres::PostgresDb,
}

impl Database {
    async fn new() -> Option<Self> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
            return None;
        };
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database("smoke")
            .ssl_mode(PgSslMode::Disable);
        let admin = PgPool::connect_with(options.clone()).await.unwrap();
        let name = format!("tooling_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone().database(&name))
            .await
            .unwrap();
        let pg = store::postgres::PostgresDb::connect(
            options
                .database(&name)
                .application_name("tooling-migrations"),
        )
        .await
        .unwrap();
        Some(Self {
            admin,
            pool,
            name,
            pg,
        })
    }

    async fn close(self) {
        // Drain every pool before dropping the database: forcibly terminated
        // backends can tear down the shared kubectl forwarding stream.
        self.pg.pool().close().await;
        self.pool.close().await;
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {}", self.name)))
            .execute(&self.admin)
            .await
            .unwrap();
        self.admin.close().await;
    }
}

#[tokio::test]
async fn concurrent_operator_runs_keep_the_connection_limit() {
    let Some(db) = Database::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("20261012000000_pause.sql"),
        "SELECT pg_sleep(2);",
    )
    .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut workers = Vec::new();
    for _ in 0..16 {
        let pg = db.pg.clone();
        let path = dir.path().to_path_buf();
        let barrier = barrier.clone();
        workers.push(tokio::spawn(async move {
            barrier.wait().await;
            pg.migrate_directory(&path).await
        }));
    }
    let mut peak = 0_i64;
    while workers.iter().any(|worker| !worker.is_finished()) {
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND application_name='tooling-migrations'")
            .fetch_one(&db.pool).await.unwrap();
        peak = peak.max(active);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    for worker in workers {
        worker.await.unwrap().unwrap();
    }
    db.close().await;
    assert!(
        peak > 1,
        "the test must observe concurrent operator connections"
    );
    assert!(
        peak <= 10,
        "operator connections exceeded the pool limit: {peak}"
    );
}

#[tokio::test]
async fn driver_preserves_types_rolls_back_and_bounds_the_pool() {
    let Some(db) = Database::new().await else {
        return;
    };
    sqlx::raw_sql(
        "CREATE TABLE sample (id UUID PRIMARY KEY, at TIMESTAMPTZ NOT NULL, body JSONB NOT NULL)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let id = Uuid::new_v4();
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-12T04:00:00-04:00")
        .unwrap()
        .to_utc();
    let body = serde_json::json!({"synthetic": true, "quantity": "0.000001"});
    let mut tx = db.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO sample VALUES ($1, $2, $3)")
        .bind(id)
        .bind(at)
        .bind(&body)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sample")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("INSERT INTO sample VALUES ($1, $2, $3)")
        .bind(id)
        .bind(at)
        .bind(&body)
        .execute(&db.pool)
        .await
        .unwrap();
    let actual: (Uuid, chrono::DateTime<chrono::Utc>, serde_json::Value) =
        sqlx::query_as("SELECT id, at, body FROM sample")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(actual, (id, at, body));
    let (left, right) = tokio::join!(db.pool.acquire(), db.pool.acquire());
    assert!(left.is_ok() && right.is_ok());
    assert_eq!(db.pool.size(), 2);
    drop((left, right));
    db.close().await;
}

#[tokio::test]
async fn timestamped_runner_serializes_checksums_and_recovers_failed_ddl() {
    let Some(db) = Database::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("20261012000000_create_sample.sql");
    std::fs::write(&first, "CREATE TABLE sample (id INT PRIMARY KEY);").unwrap();
    std::fs::write(
        dir.path().join("20261012000001_seed_sample.sql"),
        "INSERT INTO sample VALUES (1);",
    )
    .unwrap();
    let (left, right) = tokio::join!(
        db.pg.migrate_directory(dir.path()),
        db.pg.migrate_directory(dir.path())
    );
    left.unwrap();
    right.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sample")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        1
    );

    std::fs::write(
        dir.path().join("20261012000002_upgrade_sample.sql"),
        "ALTER TABLE sample ADD COLUMN note TEXT;",
    )
    .unwrap();
    db.pg.migrate_directory(dir.path()).await.unwrap();
    std::fs::write(&first, "CREATE TABLE sample (id BIGINT PRIMARY KEY);").unwrap();
    assert!(matches!(
        db.pg.migrate_directory(dir.path()).await,
        Err(store::postgres::PostgresError::Migration)
    ));
    std::fs::write(&first, "CREATE TABLE sample (id INT PRIMARY KEY);").unwrap();

    let failed = dir.path().join("20261012000003_failure.sql");
    std::fs::write(
        &failed,
        "CREATE TABLE partial_write (id INT); SELECT 1 / 0;",
    )
    .unwrap();
    assert!(db.pg.migrate_directory(dir.path()).await.is_err());
    assert!(
        sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass('partial_write')::text")
            .fetch_one(&db.pool)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        3
    );
    std::fs::write(&failed, "CREATE TABLE partial_write (id INT);").unwrap();
    db.pg.migrate_directory(dir.path()).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations WHERE success")
            .fetch_one(&db.pool)
            .await
            .unwrap(),
        4
    );
    std::fs::write(
        dir.path().join("20261012000004_owns_transaction.sql"),
        "BEGIN;\nCREATE TABLE committed_outside_runner (id INT);\nCOMMIT;",
    )
    .unwrap();
    assert!(db.pg.migrate_directory(dir.path()).await.is_err());
    assert!(sqlx::query_scalar::<_, Option<String>>(
        "SELECT to_regclass('committed_outside_runner')::text"
    )
    .fetch_one(&db.pool)
    .await
    .unwrap()
    .is_none());
    db.close().await;
}

#[tokio::test]
async fn driver_projects_age_vector_citext_and_full_text_types() {
    let Some(db) = Database::new().await else {
        return;
    };
    let mut connection = db.pool.acquire().await.unwrap();
    sqlx::raw_sql("CREATE EXTENSION age; CREATE EXTENSION vector; CREATE EXTENSION citext; LOAD 'age'; SET search_path=ag_catalog, public; SELECT create_graph('driver_graph');")
        .execute(&mut *connection).await.unwrap();
    let graph: String = sqlx::query_scalar("SELECT v::text FROM cypher('driver_graph', $$CREATE (n:Probe {name:'synthetic'}) RETURN n.name$$) AS (v agtype)")
        .fetch_one(&mut *connection).await.unwrap();
    assert_eq!(graph, "synthetic");
    let distance: f64 = sqlx::query_scalar("SELECT $1::text::vector <-> '[0,0,0]'::vector")
        .bind("[1,2,2]")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert!((distance - 3.0).abs() < f64::EPSILON);
    let case_equal: bool = sqlx::query_scalar("SELECT 'Synthetic'::citext = 'synthetic'::citext")
        .fetch_one(&mut *connection)
        .await
        .unwrap();
    assert!(case_equal);
    let matches: bool =
        sqlx::query_scalar("SELECT to_tsvector('english', $1) @@ plainto_tsquery('english', $2)")
            .bind("synthetic documents")
            .bind("document")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
    assert!(matches);
    let versions: Vec<(String, String)> = sqlx::query_as("SELECT extname, extversion FROM pg_extension WHERE extname IN ('age','vector') ORDER BY extname")
        .fetch_all(&mut *connection).await.unwrap();
    assert_eq!(
        versions,
        vec![
            ("age".into(), "1.6.0".into()),
            ("vector".into(), "0.8.2".into())
        ]
    );
    drop(connection);
    db.close().await;
}

const MIGRATIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/postgres/migrations");

type LedgerRow = (i64, String, Vec<u8>);

async fn ledger(pool: &PgPool) -> Vec<LedgerRow> {
    sqlx::query_as(
        "SELECT version, description, checksum FROM _sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn exists(pool: &PgPool, relation: &str) -> bool {
    sqlx::query_scalar::<_, Option<String>>("SELECT to_regclass($1)::text")
        .bind(relation)
        .fetch_one(pool)
        .await
        .unwrap()
        .is_some()
}

/// Columns, constraints, indexes and triggers outside the migration ledger.
async fn catalog(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT format('column %s.%s %s %s %s', table_name, column_name, data_type, is_nullable, coalesce(column_default, ''))
           FROM information_schema.columns WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'
         UNION ALL
         SELECT format('constraint %s %s', conrelid::regclass, pg_get_constraintdef(oid))
           FROM pg_constraint WHERE connamespace = 'public'::regnamespace AND conrelid <> 0
         UNION ALL
         SELECT format('index %s', indexdef) FROM pg_indexes
           WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'
         UNION ALL
         SELECT format('trigger %s %s', tgrelid::regclass, tgname) FROM pg_trigger WHERE NOT tgisinternal
         ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn checked_in_migrations_install_fresh_and_rerun_idempotently() {
    let Some(db) = Database::new().await else {
        return;
    };
    db.pg.migrate_embedded(None).await.unwrap();
    let installed = ledger(&db.pool).await;
    let expected = store::postgres::embedded_migrator()
        .iter()
        .map(|migration| {
            (
                migration.version,
                migration.description.to_string(),
                migration.checksum.to_vec(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        installed, expected,
        "every checked-in migration applies once"
    );
    for table in ["firm", "journal_entry", "journal_reconciliation_signal"] {
        assert!(exists(&db.pool, table).await, "{table} exists");
    }
    assert!(
        !exists(&db.pool, "schema_migrations").await,
        "SQLx's ledger is the only migration ledger"
    );
    let immutable: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_trigger WHERE tgname LIKE 'journal%_immutable'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(immutable, 7, "the journal migration ran under the runner");

    db.pg.migrate_embedded(None).await.unwrap();
    db.pg
        .migrate_directory(std::path::Path::new(MIGRATIONS))
        .await
        .unwrap();
    assert_eq!(
        ledger(&db.pool).await,
        installed,
        "a re-run applies nothing"
    );
    db.close().await;
}

#[tokio::test]
async fn a_schema_sql_install_runs_current_only_after_verified_adoption() {
    let Some(target) = Database::new().await else {
        return;
    };
    let Some(reference) = Database::new().await else {
        return;
    };
    sqlx::raw_sql(store::postgres_schema::canonical_schema())
        .execute(&target.pool)
        .await
        .unwrap();
    assert!(
        target.pg.migrate_embedded(None).await.is_err(),
        "an install without a ledger is never assumed current"
    );
    assert!(ledger(&target.pool).await.is_empty());

    let through = store::postgres::embedded_migrator()
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap();
    reference.pg.migrate_embedded(Some(through)).await.unwrap();
    assert_eq!(
        catalog(&target.pool).await,
        catalog(&reference.pool).await,
        "adoption requires an identical schema"
    );
    for (version, description, checksum) in ledger(&reference.pool).await {
        sqlx::query("INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES ($1, $2, true, $3, 0)")
            .bind(version)
            .bind(description)
            .bind(checksum)
            .execute(&target.pool)
            .await
            .unwrap();
    }
    target.pg.migrate_embedded(None).await.unwrap();
    assert_eq!(ledger(&target.pool).await, ledger(&reference.pool).await);
    reference.close().await;
    target.close().await;
}

#[tokio::test]
async fn template_pin_upgrade_refuses_orphans_then_preserves_repaired_rows() {
    let Some(db) = Database::new().await else {
        return;
    };
    db.pg
        .migrate_embedded(Some(20_261_016_123_000))
        .await
        .unwrap();
    let package = Uuid::new_v4();
    let template = Uuid::new_v4();
    let member = Uuid::new_v4();
    sqlx::query("INSERT INTO notation_package (id, code, title, complexity, starting_price_cents) VALUES ($1, 'synthetic', 'Synthetic', 'simple', 0)")
        .bind(package).execute(&db.pool).await.unwrap();
    sqlx::query("INSERT INTO notation_package_template (id, package_id, template_id, position) VALUES ($1, $2, $3, 0)")
        .bind(member).bind(package).bind(template).execute(&db.pool).await.unwrap();
    assert!(
        db.pg.migrate_embedded(None).await.is_err(),
        "a missing version needs reconciliation"
    );
    let applied: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = 20261016130000)",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert!(!applied, "failed constraint migration is not recorded");
    let constraints: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE conname IN ('notation_template_version', 'notation_event_template_version', 'notation_package_template_version')")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(
        constraints, 0,
        "all constraint additions roll back together"
    );
    sqlx::query("INSERT INTO template (id, code, title, respondent_type, is_current, inserted_at, updated_at) VALUES ($1, 'synthetic', 'Synthetic', 'person', false, now(), now())")
        .bind(template).execute(&db.pool).await.unwrap();
    db.pg.migrate_embedded(None).await.unwrap();
    db.pg.migrate_embedded(None).await.unwrap();
    let held: Uuid =
        sqlx::query_scalar("SELECT template_id FROM notation_package_template WHERE id = $1")
            .bind(member)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        held, template,
        "the original version pin survives the upgrade"
    );
    let validated: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_constraint WHERE conname IN ('notation_template_version', 'notation_event_template_version', 'notation_package_template_version') AND convalidated")
        .fetch_one(&db.pool).await.unwrap();
    assert_eq!(validated, 3);
    db.close().await;
}
