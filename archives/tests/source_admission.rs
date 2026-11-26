use archives::postgres::{export_configured, PostgresExport};
use cloud::StorageService;
use futures::FutureExt;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::postgres::{PostgresDb, PostgresError};

#[tokio::test]
async fn normal_archives_refuse_missing_postgres_instead_of_silently_skipping() {
    let dir = tempfile::tempdir().unwrap();
    let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
    for selected in [None, Some("postgres")] {
        let result = export_configured(
            |key| {
                (key == "NAVIGATOR_ARCHIVE_SOURCE")
                    .then(|| selected.map(str::to_owned))
                    .flatten()
            },
            &storage,
            None,
        )
        .await;
        assert!(matches!(result, Some(PostgresExport::Failed { .. })));
    }
    assert!(storage.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unknown_archive_source_cannot_skip_the_postgres_generation() {
    let dir = tempfile::tempdir().unwrap();
    let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
    for selected in ["", "postgre"] {
        let result = export_configured(
            |key| (key == "NAVIGATOR_ARCHIVE_SOURCE").then(|| selected.into()),
            &storage,
            None,
        )
        .await;
        assert!(matches!(result, Some(PostgresExport::Failed { .. })));
    }
    assert!(storage.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn explicit_compatibility_keeps_postgres_optional_without_source_or_storage_access() {
    let dir = tempfile::tempdir().unwrap();
    let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
    for host in [None, Some(" ")] {
        let result = export_configured(
            |key| match key {
                "NAVIGATOR_ARCHIVE_SOURCE" => Some("all".into()),
                "NAVIGATOR_POSTGRES_HOST" => host.map(str::to_owned),
                _ => None,
            },
            &storage,
            None,
        )
        .await;
        assert!(result.is_none());
    }
    assert!(storage.list("").await.unwrap().is_empty());
}

async fn with_unactivated_postgres<F: std::future::Future<Output = ()>>(
    test: impl FnOnce(PostgresDb) -> F,
) {
    let port = store::test_support::postgres_test_port();
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let database = format!("archive_admission_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let pg = PostgresDb::connect(options.database(&database))
        .await
        .unwrap();
    let result = std::panic::AssertUnwindSafe(async {
        pg.migrate_embedded(None).await.unwrap();
        test(pg.clone()).await;
    })
    .catch_unwind()
    .await;
    pg.pool().close().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn normal_publication_requires_handoff_but_raw_postgres_recovery_stays_available() {
    with_unactivated_postgres(|pg| async move {
        let dir = tempfile::tempdir().unwrap();
        let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
        PostgresDb::with_test_db(pg.clone(), async {
            for selected in [None, Some("postgres")] {
                let result = export_configured(
                    |key| match key {
                        "NAVIGATOR_ARCHIVE_SOURCE" => selected.map(str::to_owned),
                        "NAVIGATOR_POSTGRES_HOST" => Some("127.0.0.1".into()),
                        "NAVIGATOR_ARCHIVE_COVERAGE" => Some(
                            serde_json::json!({
                                "navigator": {"relations": []},
                                "forgejo": {"relations": []}
                            })
                            .to_string(),
                        ),
                        _ => panic!("admission must precede archive credential lookup"),
                    },
                    &storage,
                    None,
                )
                .await;
                let Some(PostgresExport::Failed { error }) = result else {
                    panic!("unenrolled PostgreSQL must not publish a scheduled generation");
                };
                assert_eq!(error, PostgresError::History.to_string());
            }
            let result = export_configured(
                |key| match key {
                    "NAVIGATOR_ARCHIVE_SOURCE" => Some("all".into()),
                    "NAVIGATOR_POSTGRES_HOST" => Some("127.0.0.1".into()),
                    "NAVIGATOR_ARCHIVE_COVERAGE" => Some(
                        serde_json::json!({
                            "navigator": {"relations": []},
                            "forgejo": {"relations": []}
                        })
                        .to_string(),
                    ),
                    _ => None,
                },
                &storage,
                None,
            )
            .await;
            let Some(PostgresExport::Failed { error }) = result else {
                panic!("raw compatibility must still require archive credentials");
            };
            assert_eq!(
                error,
                "required archive setting NAVIGATOR_ARCHIVE_FORGEJO_USER is missing"
            );
        })
        .await;
        assert!(storage.list("").await.unwrap().is_empty());

        // Raw recovery is independent of application admission and Source availability.
        sqlx::raw_sql(
            "CREATE TABLE archive_recovery_probe(id bigint PRIMARY KEY, status text); \
             INSERT INTO archive_recovery_probe VALUES(1,'synthetic')",
        )
        .execute(pg.pool())
        .await
        .unwrap();
        let generation = archives::postgres::export_generation(
            pg.pool(),
            &storage,
            &["archive_recovery_probe"],
            None,
        )
        .await
        .unwrap();
        assert_eq!(generation.tables.len(), 1);
        assert_eq!(generation.tables[0].rows, 1);
        assert!(storage.exists(&generation.manifest_key).await.unwrap());
    })
    .await;
}
