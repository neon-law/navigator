use cloud::StorageService;

#[tokio::test]
async fn enrolled_postgres_archives_need_no_surreal_coordinates() {
    store::test_support::with_postgres(async {
        for selected in [None, Some("postgres")] {
            let source = crate::runner::open_surreal_source(|key| {
                assert_eq!(key, "NAVIGATOR_ARCHIVE_SOURCE");
                selected.map(str::to_owned)
            })
            .await
            .unwrap();
            assert!(source.is_none());
            let dir = tempfile::tempdir().unwrap();
            let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
            let scheduled = "2026-10-20T05:55:00Z".parse().unwrap();
            let summary =
                crate::runner::snapshot_selected(source.as_ref(), &storage, Some(scheduled)).await;
            assert_eq!(summary.scheduled_for, Some(scheduled));
            assert_eq!(summary.run_date, scheduled.date_naive());
            assert!(summary.entries.is_empty());
            assert!(summary.failures.is_empty());
            assert!(storage.list("").await.unwrap().is_empty());
        }
    })
    .await;
}

#[tokio::test]
async fn explicit_all_keeps_the_surreal_connection_requirement() {
    let error = crate::runner::open_surreal_source(|key| {
        (key == "NAVIGATOR_ARCHIVE_SOURCE").then(|| "all".into())
    })
    .await
    .unwrap_err();
    assert!(error.to_string().contains("read SurrealDB coordinates"));
}
