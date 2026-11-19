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
