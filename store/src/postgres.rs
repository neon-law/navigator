//! PostgreSQL runtime connection for the Firm journal. Credentials are discrete
//! environment values and never form a printable connection string.
use include_dir::{include_dir, Dir};
use sqlx::migrate::{Migration, MigrationType, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use sqlx::{AssertSqlSafe, PgPool, SqlSafeStr};

pub mod people;

/// PostgreSQL Project participation ledger, preparatory to application authority.
pub mod participation;

/// Native Project records; application authority remains guarded.
pub mod projects;

/// PostgreSQL-authoritative Firm and Brand operations.
pub mod tenancy;

#[cfg(test)]
mod tenancy_tests;

#[cfg(test)]
mod projects_tests;

/// `store/postgres/migrations`, compiled in so the operator binary carries the
/// exact SQL of its release.
static MIGRATIONS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/postgres/migrations");

/// The checked-in migrations, parsed as SQLx parses a directory:
/// `<version>_<description>.sql`, with underscores read as spaces.
#[must_use]
pub fn embedded_migrator() -> Migrator {
    Migrator::with_migrations(
        MIGRATIONS
            .files()
            .filter_map(|file| {
                let name = file.path().file_name()?.to_str()?;
                let (version, rest) = name.split_once('_')?;
                let description = rest.strip_suffix(".sql")?.replace('_', " ");
                let sql = file.contents_utf8()?;
                Some(Migration::new(
                    version.parse().ok()?,
                    description.into(),
                    MigrationType::Simple,
                    AssertSqlSafe(sql).into_sql_str(),
                    sql.starts_with("-- no-transaction"),
                ))
            })
            .collect(),
    )
}
#[derive(Clone)]
pub struct PostgresDb(pub(crate) PgPool);
impl std::fmt::Debug for PostgresDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PostgresDb")
    }
}
#[derive(Debug, thiserror::Error)]
pub enum PostgresError {
    #[error("PostgreSQL configuration is unavailable or invalid")]
    Configuration,
    #[error("PostgreSQL is unavailable")]
    Connection,
    #[error("PostgreSQL migration failed")]
    Migration,
}
impl PostgresDb {
    /// The bounded pool, for read-only consumers such as the archive export.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.0
    }
    /// Apply timestamped SQL files from `path` through an explicit operator
    /// connection.
    pub async fn migrate_directory(&self, path: &std::path::Path) -> Result<(), PostgresError> {
        let runner = Migrator::new(path.to_path_buf())
            .await
            .map_err(|_| PostgresError::Migration)?;
        self.migrate(&runner, None).await
    }
    /// Apply the checked-in migrations compiled into this binary, up to and
    /// including `through` when given, so an operator Job needs no checkout.
    pub async fn migrate_embedded(&self, through: Option<i64>) -> Result<(), PostgresError> {
        self.migrate(&embedded_migrator(), through).await
    }
    /// One round trip through the pool, for readiness probes.
    pub async fn ping(&self) -> Result<(), PostgresError> {
        sqlx::query("SELECT 1")
            .execute(&self.0)
            .await
            .map(|_| ())
            .map_err(|_| PostgresError::Connection)
    }
    /// Versions recorded as successfully applied, in order.
    pub async fn applied_migrations(&self) -> Result<Vec<i64>, PostgresError> {
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success ORDER BY version")
            .fetch_all(&self.0)
            .await
            .map_err(|_| PostgresError::Migration)
    }
    /// Closing the dedicated connection releases advisory locks even when the
    /// runner returns early for checksum drift or failed DDL.
    async fn migrate(&self, runner: &Migrator, through: Option<i64>) -> Result<(), PostgresError> {
        if runner.iter().next().is_none()
            || through.is_some_and(|version| !runner.version_exists(version))
            || runner.iter().any(|migration| {
                migration.no_tx
                    || migration.sql.as_str().lines().any(|line| {
                        matches!(
                            line.trim().to_ascii_uppercase().as_str(),
                            "BEGIN;" | "COMMIT;" | "ROLLBACK;"
                        )
                    })
            })
        {
            return Err(PostgresError::Migration);
        }
        let mut connection = self
            .0
            .acquire()
            .await
            .map_err(|_| PostgresError::Connection)?;
        connection.close_on_drop();
        let result = runner
            .run_direct(through, &mut *connection, false)
            .await
            .map_err(|_| PostgresError::Migration);
        let closed = connection
            .close()
            .await
            .map_err(|_| PostgresError::Connection);
        result.and(closed)
    }
    /// Share one bounded pool per process. Configuration changes take effect on restart.
    /// Connect with verify-full TLS. Plaintext is an explicit loopback-only test option.
    pub async fn from_env() -> Result<Self, PostgresError> {
        static POOL: tokio::sync::OnceCell<PostgresDb> = tokio::sync::OnceCell::const_new();
        POOL.get_or_try_init(Self::connect_env).await.cloned()
    }
    async fn connect_env() -> Result<Self, PostgresError> {
        let get = |key: &str| std::env::var(key).map_err(|_| PostgresError::Configuration);
        let host = get("NAVIGATOR_POSTGRES_HOST")?;
        let mode =
            std::env::var("NAVIGATOR_POSTGRES_SSLMODE").unwrap_or_else(|_| "verify-full".into());
        let ssl = match mode.as_str() {
            "verify-full" => PgSslMode::VerifyFull,
            "disable" if matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1") => {
                PgSslMode::Disable
            }
            _ => return Err(PostgresError::Configuration),
        };
        let user = get("NAVIGATOR_POSTGRES_USER")?;
        if user == "postgres" && !matches!(ssl, PgSslMode::Disable) {
            return Err(PostgresError::Configuration);
        }
        let mut options = PgConnectOptions::new()
            .host(&host)
            .port(
                get("NAVIGATOR_POSTGRES_PORT")?
                    .parse()
                    .map_err(|_| PostgresError::Configuration)?,
            )
            .username(&user)
            .password(&get("NAVIGATOR_POSTGRES_PASSWORD")?)
            .database(&get("NAVIGATOR_POSTGRES_DATABASE")?)
            .ssl_mode(ssl);
        if let Ok(path) = std::env::var("NAVIGATOR_POSTGRES_ROOT_CERT") {
            options = options.ssl_root_cert(path);
        }
        Self::connect(options).await
    }
    pub async fn connect(options: PgConnectOptions) -> Result<Self, PostgresError> {
        Ok(Self(
            PgPoolOptions::new()
                .max_connections(10)
                .acquire_timeout(std::time::Duration::from_secs(10))
                .connect_with(graph_session(options))
                .await
                .map_err(|_| PostgresError::Connection)?,
        ))
    }
}

/// AGE's Cypher parser resolves through the `ag_catalog` schema, and AGE
/// queries run on any connection this store opens, so every connection adds
/// `ag_catalog` after `public` — ordinary, unqualified table resolution is
/// unchanged. The AGE library itself is preloaded per session by the role's
/// `session_preload_libraries` setting (roles.sql), not by connection
/// options: a non-superuser cannot set it at startup.
fn graph_session(options: PgConnectOptions) -> PgConnectOptions {
    options.options([("search_path", "public,ag_catalog")])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const KEYS: &[&str] = &[
        "NAVIGATOR_POSTGRES_HOST",
        "NAVIGATOR_POSTGRES_PORT",
        "NAVIGATOR_POSTGRES_USER",
        "NAVIGATOR_POSTGRES_PASSWORD",
        "NAVIGATOR_POSTGRES_DATABASE",
        "NAVIGATOR_POSTGRES_SSLMODE",
        "NAVIGATOR_POSTGRES_ROOT_CERT",
    ];

    /// A pool whose server refuses the connection pings as unavailable rather
    /// than hanging or panicking. Port 1 on loopback has no listener.
    #[tokio::test]
    async fn ping_reports_an_unreachable_server_as_unavailable() {
        let pool = PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect_lazy_with(
                PgConnectOptions::new()
                    .host("127.0.0.1")
                    .port(1)
                    .username("navigator")
                    .database("navigator")
                    .ssl_mode(PgSslMode::Disable),
            );
        assert!(matches!(
            PostgresDb(pool).ping().await,
            Err(PostgresError::Connection)
        ));
    }

    #[tokio::test]
    async fn embedded_migrations_match_the_checked_in_directory() {
        let directory = Migrator::new(std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/postgres/migrations"
        )))
        .await
        .unwrap();
        let fingerprint = |runner: &Migrator| {
            runner
                .iter()
                .map(|migration| {
                    (
                        migration.version,
                        migration.description.to_string(),
                        migration.checksum.to_vec(),
                        migration.no_tx,
                    )
                })
                .collect::<Vec<_>>()
        };
        let embedded = embedded_migrator();
        assert!(embedded.iter().next().is_some());
        assert_eq!(fingerprint(&embedded), fingerprint(&directory));
    }

    #[tokio::test]
    async fn firm_secret_migration_upgrades_a_database_with_purchase_applied() {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "required PostgreSQL upgrade test lacks its disposable port"
            );
            return;
        };
        let port = port
            .parse::<u16>()
            .expect("test PostgreSQL port is numeric");
        let options = |database: &str| {
            PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username("postgres")
                .password("synthetic")
                .database(database)
                .ssl_mode(PgSslMode::Disable)
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options("postgres"))
            .await
            .expect("connect to required synthetic PostgreSQL fixture");
        // This identifier contains only a fixed prefix and UUID hex digits.
        let database = format!(
            "navigator_purchase_upgrade_{}",
            uuid::Uuid::new_v4().simple()
        );
        let create_database = format!("CREATE DATABASE \"{database}\"");
        sqlx::query(AssertSqlSafe(create_database))
            .execute(&admin)
            .await
            .expect("create isolated upgrade database");
        // The graph migration requires the AGE extension; production creates it
        // in the privileged init path before any migration runs.
        let fixture = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options(&database))
            .await
            .expect("connect to isolated upgrade database");
        sqlx::query("CREATE EXTENSION IF NOT EXISTS age")
            .execute(&fixture)
            .await
            .expect("create AGE extension on isolated upgrade database");
        fixture.close().await;
        let result = verify_purchase_upgrade(&options(&database)).await;
        let drop_database = format!("DROP DATABASE \"{database}\" WITH (FORCE)");
        sqlx::query(AssertSqlSafe(drop_database))
            .execute(&admin)
            .await
            .expect("drop isolated upgrade database");
        admin.close().await;
        result.expect("embedded Firm secret migration upgrades Purchase database");
    }

    async fn verify_purchase_upgrade(
        options: &PgConnectOptions,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let db = PostgresDb::connect(options.clone()).await?;
        let applied_release = Migrator::with_migrations(
            embedded_migrator()
                .iter()
                .filter(|migration| {
                    migration.version <= 20_261_020_120_000
                        && migration.version != 20_261_020_100_000
                })
                .cloned()
                .collect(),
        );
        applied_release.run(&db.0).await?;
        let purchase_checksum: Vec<u8> = sqlx::query_scalar(
            "SELECT checksum FROM _sqlx_migrations WHERE version = 20261020120000",
        )
        .fetch_one(&db.0)
        .await?;

        db.migrate_embedded(None).await?;
        db.migrate_embedded(None).await?;

        let applied = db.applied_migrations().await?;
        assert!(applied.contains(&20_261_020_100_000));
        assert!(applied.contains(&20_261_020_120_000));
        let actual_purchase_checksum: Vec<u8> = sqlx::query_scalar(
            "SELECT checksum FROM _sqlx_migrations WHERE version = 20261020120000",
        )
        .fetch_one(&db.0)
        .await?;
        assert_eq!(actual_purchase_checksum, purchase_checksum);
        let secret_tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = current_schema() AND table_name = 'firm_integration_secret'",
        )
        .fetch_one(&db.0)
        .await?;
        assert_eq!(secret_tables, 1);
        db.0.close().await;
        Ok(())
    }

    // Environment and the OnceCell belong to each child process. No test mutates
    // another test's environment, and this remains safe under Cargo and nextest.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "isolated configuration matrix keeps each credential and TLS scenario together"
    )]
    fn runtime_configuration_isolated() {
        let mut cases = vec![
            "host",
            "port",
            "user",
            "password",
            "database",
            "invalid_port",
            "invalid_ssl",
            "remote_plaintext",
            "bootstrap_tls",
            "connection",
            "root_cert",
        ];
        if std::env::var_os("NAVIGATOR_TEST_POSTGRES_PORT").is_some() {
            cases.push("shared_pool");
        } else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "required PostgreSQL connection test lacks its disposable port"
            );
        }
        for case in cases {
            let mut child = Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "postgres::tests::isolated_configuration_probe",
                    "--nocapture",
                ])
                .env("NAVIGATOR_POSTGRES_TEST_CASE", case);
            for key in KEYS {
                child.env_remove(key);
            }
            child
                .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
                .env("NAVIGATOR_POSTGRES_PORT", "1")
                .env("NAVIGATOR_POSTGRES_USER", "synthetic-reader")
                .env(
                    "NAVIGATOR_POSTGRES_PASSWORD",
                    "synthetic-secret-must-not-appear",
                )
                .env("NAVIGATOR_POSTGRES_DATABASE", "synthetic-database")
                .env("NAVIGATOR_POSTGRES_SSLMODE", "disable");
            match case {
                "host" => {
                    child.env_remove(KEYS[0]);
                }
                "port" => {
                    child.env_remove(KEYS[1]);
                }
                "user" => {
                    child.env_remove(KEYS[2]);
                }
                "password" => {
                    child.env_remove(KEYS[3]);
                }
                "database" => {
                    child.env_remove(KEYS[4]);
                }
                "invalid_port" => {
                    child.env(KEYS[1], "not-a-port");
                }
                "invalid_ssl" => {
                    child.env(KEYS[5], "prefer");
                }
                "remote_plaintext" => {
                    child.env(KEYS[0], "synthetic.example.invalid");
                }
                "bootstrap_tls" => {
                    child.env(KEYS[2], "postgres").env_remove(KEYS[5]);
                }
                "root_cert" => {
                    child
                        .env_remove(KEYS[5])
                        .env(KEYS[6], "missing-synthetic-postgres-ca.pem");
                }
                "shared_pool" => {
                    child
                        .env(
                            KEYS[1],
                            std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
                        )
                        .env(KEYS[2], "postgres")
                        .env(KEYS[3], "synthetic")
                        .env(
                            KEYS[4],
                            std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                                .unwrap_or_else(|_| "smoke".into()),
                        );
                }
                "connection" => {}
                _ => unreachable!(),
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "isolated scenario {case}: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[tokio::test]
    async fn isolated_configuration_probe() {
        let Ok(case) = std::env::var("NAVIGATOR_POSTGRES_TEST_CASE") else {
            return;
        };
        if case == "shared_pool" {
            let (first, second) = tokio::join!(PostgresDb::from_env(), PostgresDb::from_env());
            let first = first.unwrap();
            let second = second.unwrap();
            let answer: i32 = sqlx::query_scalar("SELECT 42")
                .fetch_one(&first.0)
                .await
                .unwrap();
            assert_eq!(answer, 42);
            assert_eq!(format!("{first:?}"), "PostgresDb");
            first.0.close().await;
            assert!(
                second.0.is_closed(),
                "both handles must share the bounded pool"
            );
            let error = second.migrate_embedded(None).await.unwrap_err();
            assert!(matches!(error, PostgresError::Connection));
            assert_eq!(error.to_string(), "PostgreSQL is unavailable");
        } else {
            let error = PostgresDb::from_env().await.unwrap_err();
            if matches!(case.as_str(), "connection" | "root_cert") {
                assert!(matches!(error, PostgresError::Connection));
                assert_eq!(error.to_string(), "PostgreSQL is unavailable");
            } else {
                assert!(matches!(error, PostgresError::Configuration));
                assert_eq!(
                    error.to_string(),
                    "PostgreSQL configuration is unavailable or invalid"
                );
            }
            for display in [error.to_string(), format!("{error:?}")] {
                assert!(!display.contains("synthetic-secret"));
                assert!(!display.contains("synthetic-database"));
                assert!(!display.contains("synthetic-reader"));
            }
        }
    }
}
