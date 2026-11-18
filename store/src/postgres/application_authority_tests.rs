use super::{PostgresDb, PostgresError};

#[tokio::test]
async fn absent_postgres_configuration_cannot_select_source() {
    const CHILD: &str = "NAVIGATOR_TEST_APPLICATION_CONFIGURATION";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "postgres::application_authority_tests::absent_postgres_configuration_cannot_select_source", "--nocapture"])
            .env(CHILD, "child")
            .env_remove("NAVIGATOR_POSTGRES_HOST")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(matches!(
        PostgresDb::configured().await,
        Err(PostgresError::Configuration)
    ));
    let source = crate::surreal::SurrealDb::uninitialized();
    assert!(matches!(
        crate::persons::find_by_id(&source, uuid::Uuid::nil()).await,
        Err(crate::persons::PersonError::Postgres(_))
    ));
}

#[tokio::test]
async fn application_history_rejects_unenrolled_database() {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL application fixture");
    let pg = crate::postgres_mirror::tests::connect(&options, &database).await;
    assert!(matches!(
        pg.require_application_history().await,
        Err(PostgresError::History)
    ));
}
