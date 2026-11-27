//! Native test fixtures must carry real creation authority.
use futures::FutureExt as _;
use store::application_store::ApplicationStore;
use store::{postgres::PostgresDb, test_support};

/// A native actor fixture is admitted by its real creation origin, never by
/// fabricated global or scoped captured-history completion rows.
#[tokio::test]
async fn default_postgres_fixture_has_genuine_native_origin_and_no_capture_ledger() {
    test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let identity: (String, i64, Option<String>) = sqlx::query_as(
            "SELECT current_database(), oid::bigint, shobj_description(oid, 'pg_database') \
             FROM pg_database WHERE datname = current_database()",
        )
        .fetch_one(pg.pool())
        .await
        .unwrap();
        let origin: Option<(String, uuid::Uuid, String, i64)> = sqlx::query_as(
            "SELECT origin, creation_operation, database_name, database_oid::bigint \
             FROM database_origin WHERE singleton",
        )
        .fetch_optional(pg.pool())
        .await
        .unwrap();
        assert!(
            origin.is_some(),
            "a native fixture requires an actual Database Origin"
        );
        let (kind, operation, database, oid) = origin.unwrap();
        assert_eq!(kind, "fresh_native");
        assert_eq!(
            store::postgres::native_origin::database_name(operation).unwrap(),
            database
        );
        assert_eq!(database, identity.0);
        assert_eq!(oid, identity.1);
        assert_eq!(
            identity.2,
            Some(format!("navigator-native-origin-v1:{operation}"))
        );
        let ledger: (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM mirror_backfill_run WHERE scope = 'global'), \
                    (SELECT count(*) FROM mirror_backfill_run WHERE scope <> 'global'), \
                    (SELECT count(*) FROM mirror_backfill_checkpoint), \
                    (SELECT count(*) FROM mirror_backfill_failure)",
        )
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert_eq!(
            ledger,
            (0, 0, 0, 0),
            "native creation cannot fabricate captured-history completion"
        );
        PostgresDb::with_application_db(pg.clone(), async {
            let admitted = PostgresDb::application().await.unwrap();
            let actual: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(admitted.pool())
                .await
                .unwrap();
            assert_eq!(actual, database);
        })
        .await
        .expect("the genuine native origin admits application work");
    })
    .await;
}

#[tokio::test]
async fn fresh_postgres_fixtures_keep_native_rows_isolated_and_cleanup_owned_database() {
    test_support::with_postgres(async {
        let first = PostgresDb::application().await.unwrap();
        let options = (*first.pool().connect_options()).clone();
        let second = test_support::PostgresFixture::create(options)
            .await
            .unwrap();
        let name = second.database().to_owned();
        let source = ApplicationStore::uninitialized();
        store::persons::create(
            &source,
            &store::persons::NewPerson::with_role(
                "Synthetic Isolated Person",
                "isolated-fixture@example.test",
                store::persons::Role::Client,
            ),
        )
        .await
        .unwrap();
        PostgresDb::with_test_db(second.postgres().clone(), async {
            assert!(
                store::persons::find_by_email_ci(&source, "isolated-fixture@example.test")
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
            store::persons::find_by_email_ci(&source, "isolated-fixture@example.test")
                .await
                .unwrap()
                .is_some()
        );
    })
    .await;
}
#[tokio::test]
async fn postgres_actor_fixture_cleans_up_after_a_test_panics() {
    test_support::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let database = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded = database.clone();
        let result = std::panic::AssertUnwindSafe(test_support::with_postgres(async move {
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
#[tokio::test]
async fn independently_routed_fixture_options_preserve_owned_cleanup() {
    test_support::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let options = (*parent.pool().connect_options()).clone();
        let admin_tag = format!("fixture_admin_{}", uuid::Uuid::now_v7().simple());
        let data_tag = format!("fixture_data_{}", uuid::Uuid::now_v7().simple());
        let fixture = test_support::PostgresFixture::create_with_connections(
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
    test_support::with_postgres(async {
        let parent = PostgresDb::application().await.unwrap();
        let options = (*parent.pool().connect_options()).clone();
        let missing_user = format!("absent_fixture_{}", uuid::Uuid::now_v7().simple());
        let error = match test_support::PostgresFixture::create_with_connections(
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
        let operation = database
            .strip_prefix("navigator_native_")
            .and_then(|suffix| uuid::Uuid::parse_str(suffix).ok())
            .expect("the fixture error names its exact native creation operation");
        assert_eq!(
            store::postgres::native_origin::database_name(operation).unwrap(),
            database
        );
        assert!(matches!(
            error.downcast_ref::<store::postgres::PostgresError>(),
            Some(store::postgres::PostgresError::Connection)
        ));
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
