use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use uuid::Uuid;
use workflows::dbos_engine::{self, Settings};

/// Nothing legitimate here takes this long; a hang fails instead of holding CI.
pub const DEADLINE: Duration = Duration::from_mins(1);

/// The application version every test launches with, so a relaunch recovers
/// what the previous launch left pending.
pub const APP_VERSION: &str = "test";

/// A fresh synthetic database on the test PostgreSQL server.
pub struct TestDatabase {
    /// The DBOS worker's environment for this database, as `serve` reads it.
    pub env: Vec<(&'static str, String)>,
    /// The connection [`Settings`] builds from `env`.
    pub url: String,
    pub pool: sqlx::PgPool,
}

impl TestDatabase {
    /// The store's own PostgreSQL handle on this database, migrated, for the
    /// production work sources that read the durable queues.
    pub async fn store(&self) -> store::postgres::PostgresDb {
        let lookup = self.lookup();
        let get = |key| lookup(key).expect("test database setting");
        let options = PgConnectOptions::new()
            .host(&get(dbos_engine::HOST_ENV))
            .port(get(dbos_engine::PORT_ENV).parse().expect("port"))
            .username(&get(dbos_engine::USER_ENV))
            .password(&get(dbos_engine::PASSWORD_ENV))
            .database(&get(dbos_engine::DATABASE_ENV))
            .ssl_mode(PgSslMode::Disable);
        let pg = store::postgres::PostgresDb::connect(options)
            .await
            .expect("connect the store to the test database");
        pg.migrate_embedded(None)
            .await
            .expect("migrate the test database");
        pg
    }

    pub fn lookup(&self) -> impl Fn(&str) -> Option<String> + 'static {
        let env = self.env.clone();
        move |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
        }
    }
}

/// Create a fresh database, or `None` when no test PostgreSQL is configured.
pub async fn database() -> Option<TestDatabase> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let port: u16 = port
        .parse()
        .expect("NAVIGATOR_TEST_POSTGRES_PORT is a port");
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone())
        .await
        .expect("connect to the test server");
    let name = format!("dbos_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .expect("create the test database");
    admin.close().await;
    let pool = sqlx::PgPool::connect_with(options.database(&name))
        .await
        .expect("connect to the test database");
    let mut db = TestDatabase {
        env: vec![
            (dbos_engine::HOST_ENV, "127.0.0.1".to_owned()),
            (dbos_engine::PORT_ENV, port.to_string()),
            (dbos_engine::USER_ENV, "postgres".to_owned()),
            (dbos_engine::PASSWORD_ENV, "synthetic".to_owned()),
            (dbos_engine::DATABASE_ENV, name),
            (dbos_engine::SSLMODE_ENV, "disable".to_owned()),
            (dbos_engine::RELEASE_ENV, APP_VERSION.to_owned()),
            (
                dbos_engine::EXECUTOR_ID_ENV,
                "navigator-test-pod".to_owned(),
            ),
        ],
        url: String::new(),
        pool,
    };
    db.url = Settings::from_lookup(db.lookup())
        .expect("complete test settings")
        .config()
        .database_url;
    Some(db)
}

/// A synthetic Firm, its admin, and one open Project on it.
pub struct FirmMatter {
    pub admin: Uuid,
    pub firm: Uuid,
    pub project: Uuid,
}

/// Seed a [`FirmMatter`] in `surreal`.
pub async fn firm_matter(surreal: &store::surreal::SurrealDb, label: &str) -> FirmMatter {
    use store::persons::{NewPerson, Role};
    let admin = store::persons::create(
        surreal,
        &NewPerson::with_role(
            format!("{label} Admin"),
            format!(
                "{}-admin-{}@example.com",
                label.to_lowercase(),
                Uuid::now_v7()
            ),
            Role::Admin,
        ),
    )
    .await
    .expect("seed the Firm admin");
    let firm = store::firms::create(
        surreal,
        &store::firms::NewFirm {
            name: format!("{label} Synthetic Firm"),
            status: "active".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed the Firm");
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: format!(
                "{}-{}",
                label.to_lowercase(),
                &Uuid::now_v7().simple().to_string()[..8]
            ),
            name: format!("Synthetic {label} Matter"),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .expect("seed the Project");
    FirmMatter {
        admin: admin.id,
        firm: firm.id,
        project: project.id,
    }
}
