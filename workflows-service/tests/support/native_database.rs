use sqlx::postgres::{PgConnectOptions, PgSslMode};
use workflows::dbos_engine::{self, Settings};

/// The application version every test launches with, so a relaunch recovers
/// what the previous launch left pending.
pub const APP_VERSION: &str = "test";

/// A fresh synthetic database on the test PostgreSQL server.
pub struct TestDatabase {
    /// The DBOS worker's environment for this database, as `serve` reads it.
    pub env: Vec<(&'static str, String)>,
    /// The connection [`Settings`] builds from `env`.
    pub url: String,
    fixture: store::test_support::PostgresFixture,
}

impl TestDatabase {
    /// The store's own PostgreSQL handle on this database, migrated, for the
    /// production work sources that read the durable queues.
    pub fn store(&self) -> store::postgres::PostgresDb {
        self.fixture.postgres().clone()
    }

    /// Close only this helper's exclusively owned resource after its workers drain.
    pub async fn close(self) {
        self.fixture
            .close()
            .await
            .expect("close owned DBOS fixture");
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
    database_with_activation(true).await
}

/// Create the same owned fixture with or without application enrollment.
pub async fn database_with_activation(activated: bool) -> Option<TestDatabase> {
    let port = store::test_support::postgres_test_port();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable)
        .options([("session_preload_libraries", "age")]);
    let fixture = if activated {
        store::test_support::PostgresFixture::create(options).await
    } else {
        store::test_support::PostgresFixture::create_unactivated(options).await
    }
    .expect("create exclusively fresh DBOS fixture");
    let name = fixture.database().to_owned();
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
        fixture,
    };
    db.url = Settings::from_lookup(db.lookup())
        .expect("complete test settings")
        .config()
        .database_url;
    Some(db)
}
