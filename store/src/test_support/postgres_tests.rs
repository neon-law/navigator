use crate::{postgres::PostgresDb, surreal::SurrealDb};
use futures::FutureExt as _;

#[tokio::test]
async fn fresh_postgres_fixtures_keep_native_rows_isolated_and_cleanup_owned_database() {
    super::with_postgres(async {
        let first = PostgresDb::application().await.unwrap();
        let options = (*first.pool().connect_options()).clone();
        let second = super::PostgresFixture::create(options).await.unwrap();
        let name = second.database().to_owned();
        let source = SurrealDb::uninitialized();
        crate::persons::create(
            &source,
            &crate::persons::NewPerson::with_role(
                "Synthetic Isolated Person",
                "isolated-fixture@example.test",
                crate::persons::Role::Client,
            ),
        )
        .await
        .unwrap();
        PostgresDb::with_test_db(second.postgres().clone(), async {
            assert!(
                crate::persons::find_by_email_ci(&source, "isolated-fixture@example.test")
                    .await
                    .unwrap()
                    .is_none()
            );
        })
        .await;
        second.close().await.unwrap();
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(&name)
                .fetch_one(first.pool())
                .await
                .unwrap();
        assert!(!exists);
        assert!(
            crate::persons::find_by_email_ci(&source, "isolated-fixture@example.test")
                .await
                .unwrap()
                .is_some()
        );
    })
    .await;
}

#[tokio::test]
async fn postgres_actor_fixture_cleans_up_after_a_test_panics() {
    super::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let database = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded = database.clone();
        let result = std::panic::AssertUnwindSafe(super::with_postgres(async move {
            let pg = PostgresDb::application().await.unwrap();
            let name: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(pg.pool())
                .await
                .unwrap();
            *recorded.lock().unwrap() = Some(name);
            panic!("synthetic actor failure");
        }))
        .catch_unwind()
        .await;
        assert!(result.is_err());
        let name = database.lock().unwrap().take().unwrap();
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(name)
                .fetch_one(parent.pool())
                .await
                .unwrap();
        assert!(!exists);
        assert_eq!(
            PostgresDb::application()
                .await
                .unwrap()
                .pool()
                .connect_options()
                .get_database(),
            parent.pool().connect_options().get_database()
        );
    })
    .await;
}

#[test]
fn failed_fixture_cleanup_preserves_the_original_setup_cause() {
    let error = super::fixture_setup_failure(
        anyhow::anyhow!("synthetic schema setup failure"),
        Err(anyhow::anyhow!("synthetic cleanup transport failure")),
    );
    assert_eq!(
        error.root_cause().to_string(),
        "synthetic schema setup failure"
    );
    assert!(format!("{error:#}").contains("synthetic cleanup transport failure"));
    let clean = super::fixture_setup_failure(anyhow::anyhow!("synthetic setup cause"), Ok(()));
    assert_eq!(clean.root_cause().to_string(), "synthetic setup cause");
    assert!(!format!("{clean:#}").contains("cleanup"));
}

#[tokio::test]
async fn independently_routed_fixture_options_preserve_owned_cleanup() {
    super::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let options = (*parent.pool().connect_options()).clone();
        let admin_tag = format!("fixture_admin_{}", uuid::Uuid::now_v7().simple());
        let data_tag = format!("fixture_data_{}", uuid::Uuid::now_v7().simple());
        let fixture = super::PostgresFixture::create_with_connections(
            options.clone().application_name(&admin_tag),
            options.application_name(&data_tag),
        )
        .await
        .unwrap();
        let database = fixture.database().to_owned();
        let actual_data: String = sqlx::query_scalar("SELECT current_setting('application_name')")
            .fetch_one(fixture.postgres().pool())
            .await
            .unwrap();
        assert_eq!(actual_data, data_tag);
        let admin_databases: Vec<String> =
            sqlx::query_scalar("SELECT datname FROM pg_stat_activity WHERE application_name=$1")
                .bind(&admin_tag)
                .fetch_all(parent.pool())
                .await
                .unwrap();
        assert!(!admin_databases.is_empty());
        assert!(admin_databases
            .iter()
            .all(|name| name == parent.pool().connect_options().get_database().unwrap()));
        fixture.close().await.unwrap();
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(database)
                .fetch_one(parent.pool())
                .await
                .unwrap();
        assert!(!exists);
    })
    .await;
}

#[tokio::test]
async fn failed_data_connection_drops_only_the_minted_fixture_database() {
    super::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let options = (*parent.pool().connect_options()).clone();
        let missing_user = format!("absent_fixture_{}", uuid::Uuid::now_v7().simple());
        let error = match super::PostgresFixture::create_with_connections(
            options.clone(),
            options.username(&missing_user),
        )
        .await
        {
            Ok(fixture) => {
                fixture.close().await.unwrap();
                panic!("a nonexistent data login unexpectedly connected");
            }
            Err(error) => error,
        };
        let database = error
            .chain()
            .find_map(|cause| {
                cause
                    .to_string()
                    .strip_prefix("initialize owned synthetic fixture ")
                    .map(str::to_owned)
            })
            .unwrap();
        assert!(database.starts_with("native_test_"));
        assert!(error.root_cause().to_string().contains(&missing_user));
        assert!(!format!("{error:#}").contains("cleanup also failed"));
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
                .bind(database)
                .fetch_one(parent.pool())
                .await
                .unwrap();
        assert!(!exists);
        let parent_database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(parent.pool())
            .await
            .unwrap();
        assert_eq!(
            Some(parent_database.as_str()),
            parent.pool().connect_options().get_database()
        );
    })
    .await;
}
