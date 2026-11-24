//! A private process lets DBOS-spawned tasks connect to their test's application database.

use store::postgres::PostgresDb;
use workflows::dbos_engine::Settings;

pub struct ApplicationDatabase {
    pub url: String,
    postgres: PostgresDb,
}

impl ApplicationDatabase {
    pub fn store(&self) -> &PostgresDb {
        &self.postgres
    }
}

/// In the parent, run this exact test in a child bound to an owned PostgreSQL database.
/// The child exercises both the application's environment lookup and DBOS recovery.
pub async fn database() -> Option<ApplicationDatabase> {
    const CHILD: &str = "NAVIGATOR_DBOS_APPLICATION_TEST";
    let name = std::thread::current()
        .name()
        .expect("named test thread")
        .to_owned();
    if std::env::var(CHILD).as_ref() == Ok(&name) {
        let url = Settings::from_lookup(|key| std::env::var(key).ok())
            .expect("child DBOS settings")
            .config()
            .database_url;
        return Some(ApplicationDatabase {
            url,
            postgres: PostgresDb::application()
                .await
                .expect("admitted application store"),
        });
    }

    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let options = pg.pool().connect_options();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, &name);
        for prefix in ["NAVIGATOR_POSTGRES", "NAVIGATOR_DBOS"] {
            for (key, value) in [
                ("HOST", options.get_host().to_owned()),
                ("PORT", options.get_port().to_string()),
                ("DATABASE", options.get_database().unwrap().to_owned()),
                ("USER", options.get_username().to_owned()),
                ("PASSWORD", "synthetic".to_owned()),
                ("SSLMODE", "disable".to_owned()),
            ] {
                command.env(format!("{prefix}_{key}"), value);
            }
        }
        let mut child = command
            .env("NAVIGATOR_DBOS_EXECUTOR_ID", "native-test")
            .env("NAVIGATOR_RELEASE_TAG", "test")
            .kill_on_drop(true)
            .spawn()
            .expect("start native DBOS test");
        match tokio::time::timeout(std::time::Duration::from_secs(180), child.wait()).await {
            Ok(status) => assert!(
                status.unwrap().success(),
                "{name} failed in its owned database"
            ),
            Err(_) => {
                child.kill().await.expect("stop timed-out worker");
                panic!("{name} timed out in its owned database");
            }
        }
    })
    .await;
    None
}
