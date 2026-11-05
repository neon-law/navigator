use super::*;
use chrono::{Days, TimeDelta, TimeZone};
use cloud::FsStorage;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::sent_emails::NewSentEmail;

const BASE: &str = "gs://synthetic-archives";

/// A fresh database with the canonical schema: the store handle the exporter
/// uses, and a raw pool for the assertions it cannot make.
async fn database() -> Option<(PostgresDb, PgPool)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL archive tests require NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone().database(
        &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
    ))
    .await
    .unwrap();
    let database = format!("sent_email_export_{}", uuid::Uuid::now_v7().simple());
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let options = options
        .database(&database)
        .options([("session_preload_libraries", "age")]);
    let setup = PgPool::connect_with(options.clone()).await.unwrap();
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::raw_sql(store::postgres_schema::canonical_schema())
        .execute(&setup)
        .await
        .unwrap();
    Some((PostgresDb::connect(options).await.unwrap(), setup))
}

async fn send(pg: &PostgresDb, sent_at: DateTime<Utc>) -> SentEmail {
    log::record(
        pg,
        &NewSentEmail {
            recipient: "recipient@example.com".into(),
            subject: "Synthetic subject".into(),
            sender: "sender@example.com".into(),
            project_id: None,
            person_id: Some(uuid::Uuid::now_v7()),
            template_slug: Some("welcome".into()),
            body: "Synthetic body".into(),
            outcome: "sent".into(),
            sg_message_id: Some("synthetic-receipt".into()),
            sent_at,
        },
    )
    .await
    .unwrap()
}

fn noon(day: NaiveDate) -> DateTime<Utc> {
    day.and_hms_opt(12, 0, 0).unwrap().and_utc()
}

async fn table(storage: &FsStorage) -> Current {
    current(storage, BASE).await.unwrap().expect("table exists")
}

#[tokio::test]
async fn a_day_lands_once_in_a_day_partitioned_table_and_a_rerun_writes_nothing() {
    let Some((pg, _raw)) = database().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(dir.path()).await.unwrap();
    let now = Utc::now();
    let day = now.date_naive() - Days::new(1);
    let written = send(&pg, noon(day)).await;

    assert_eq!(
        log::pending_days(&pg, now.date_naive()).await.unwrap(),
        vec![day]
    );
    let first = export_day(&pg, &storage, BASE, day, now).await.unwrap();
    assert!(
        matches!(first, DayOutcome::Committed { rows: 1, .. }),
        "{first:?}"
    );
    assert!(log::pending_days(&pg, now.date_naive())
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        export_day(&pg, &storage, BASE, day, now).await.unwrap(),
        DayOutcome::Unchanged { rows: 1 }
    );

    let current = table(&storage).await;
    assert_eq!(
        current.version, 1,
        "the re-run committed no second snapshot"
    );
    assert_eq!(current.manifests.len(), 1);
    let spec = current.metadata.default_partition_spec();
    assert_eq!(spec.fields().len(), 1);
    assert_eq!(spec.fields()[0].name, PARTITION_FIELD);
    assert_eq!(spec.fields()[0].transform, Transform::Day);

    let ledger = log::export_for(&pg, day).await.unwrap().unwrap();
    assert_eq!(ledger.row_count, 1);
    let key = key_of(BASE, &ledger.data_file).unwrap();
    assert!(key.starts_with(&format!("{TABLE_PREFIX}/data/sent_at_day={day}/")));
    let bytes = storage.get(key).await.unwrap().bytes;
    let batches: Vec<_> = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes))
        .unwrap()
        .build()
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let ids = batches[0]
        .column_by_name("id")
        .unwrap()
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap();
    assert_eq!(ids.value(0), written.id.to_string());
    assert!(batches[0].column_by_name("person_id").is_some());
}

#[tokio::test]
async fn each_snapshot_carries_earlier_days_and_a_late_row_replaces_only_its_day() {
    let Some((pg, _raw)) = database().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(dir.path()).await.unwrap();
    let now = Utc::now();
    let older = now.date_naive() - Days::new(3);
    let newer = now.date_naive() - Days::new(2);
    send(&pg, noon(older)).await;
    send(&pg, noon(newer)).await;
    for day in log::pending_days(&pg, now.date_naive()).await.unwrap() {
        export_day(&pg, &storage, BASE, day, now).await.unwrap();
    }
    let both = table(&storage).await;
    assert_eq!(both.version, 2);
    assert_eq!(
        both.manifests.len(),
        2,
        "the second snapshot lists both days"
    );

    send(&pg, noon(older) + TimeDelta::hours(1)).await;
    assert_eq!(
        log::pending_days(&pg, now.date_naive()).await.unwrap(),
        vec![older]
    );
    let late = export_day(&pg, &storage, BASE, older, now).await.unwrap();
    assert!(
        matches!(late, DayOutcome::Committed { rows: 2, .. }),
        "{late:?}"
    );
    let replaced = table(&storage).await;
    assert_eq!(replaced.version, 3);
    assert_eq!(
        replaced.manifests.len(),
        2,
        "the old day's manifest was replaced"
    );
    let summary = replaced.metadata.current_snapshot().unwrap().summary();
    assert_eq!(summary.operation, Operation::Overwrite);
    let older_manifests = replaced
        .manifests
        .iter()
        .filter(|m| m.manifest_path.contains(&day_manifest_marker(older)))
        .count();
    assert_eq!(older_manifests, 1);
}

#[tokio::test]
async fn a_commit_whose_ledger_write_was_lost_is_recorded_without_a_second_snapshot() {
    let Some((pg, raw)) = database().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(dir.path()).await.unwrap();
    let now = Utc::now();
    let day = now.date_naive() - Days::new(1);
    send(&pg, noon(day)).await;
    export_day(&pg, &storage, BASE, day, now).await.unwrap();
    sqlx::query("DELETE FROM sent_email_export WHERE day = $1")
        .bind(day)
        .execute(&raw)
        .await
        .unwrap();

    assert_eq!(
        export_day(&pg, &storage, BASE, day, now).await.unwrap(),
        DayOutcome::Recorded { rows: 1 }
    );
    assert_eq!(table(&storage).await.version, 1);
    assert!(log::export_for(&pg, day).await.unwrap().is_some());
}

#[tokio::test]
async fn the_run_purges_an_exported_day_past_the_year_but_the_table_keeps_it() {
    let Some((pg, raw)) = database().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    let now = Utc::now();
    let (_, last_day) = log::retention_cutoff(now);
    let old_day = last_day - Days::new(2);
    send(&pg, noon(old_day)).await;
    send(&pg, Utc::now()).await;
    let source = StoreExportSource {
        pg,
        storage: Arc::clone(&storage),
        location_base: BASE.into(),
    };

    let days = source.pending(now).await.unwrap();
    assert_eq!(
        days,
        vec![old_day],
        "today's rows wait for the day to close"
    );
    for day in days {
        source.export(day, now).await.unwrap();
    }
    let purged = source.purge(now).await.unwrap();
    assert_eq!(purged.len(), 1);
    assert!(purged[0].1.starts_with("purged 1 rows"), "{purged:?}");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM sent_email")
        .fetch_one(&raw)
        .await
        .unwrap();
    assert_eq!(left, 1, "only this year's row stays in PostgreSQL");
    let current = current(storage.as_ref(), BASE).await.unwrap().unwrap();
    assert_eq!(
        current.manifests.len(),
        1,
        "the purged day is still in Iceberg"
    );
    assert!(source.pending(now).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_retention_locked_bucket_gets_only_new_objects_and_no_version_hint() {
    let Some((pg, _raw)) = database().await else {
        return;
    };
    let storage = crate::test_support::CollisionStorage::locked();
    let now = Utc::now();
    let day = now.date_naive() - Days::new(2);
    send(&pg, noon(day)).await;
    // `CollisionStorage::locked` panics on a plain `put` or `delete`, and its
    // `put_immutable` refuses an existing key.
    export_day(&pg, storage.as_ref(), BASE, day, now)
        .await
        .unwrap();
    send(&pg, noon(day) + TimeDelta::hours(1)).await;
    let late = export_day(&pg, storage.as_ref(), BASE, day, now)
        .await
        .unwrap();
    assert!(
        matches!(late, DayOutcome::Committed { rows: 2, .. }),
        "{late:?}"
    );

    assert!(!storage.contains(&format!("{TABLE_PREFIX}/metadata/version-hint.text")));
    let current = current(storage.as_ref(), BASE).await.unwrap().unwrap();
    assert_eq!(current.version, 2, "the reader found v2 without a hint");
    assert_eq!(current.manifests.len(), 1, "the late row replaced its day");
    assert_eq!(
        export_day(&pg, storage.as_ref(), BASE, day, now)
            .await
            .unwrap(),
        DayOutcome::Unchanged { rows: 2 }
    );
}

#[test]
fn the_schedule_has_one_slot_a_day_and_only_true_enables_it() {
    let slot = daily::slot(
        Utc.with_ymd_and_hms(2026, 11, 4, 0, 0, 0).unwrap(),
        slot_time(),
    );
    assert_eq!(slot, Utc.with_ymd_and_hms(2026, 11, 4, 2, 41, 0).unwrap());
    assert_eq!(workflow_id(slot), "SentEmailExport-2026-11-04");
    assert_eq!(slot_of("SentEmailExport-2026-11-04"), Some(slot));
    assert_eq!(slot_of("Archives-2026-11-04"), None);
    assert!(!enabled(|_| None).unwrap());
    assert!(enabled(|_| Some(" true ".into())).unwrap());
    assert!(enabled(|_| Some("1".into())).is_err());
}

#[test]
fn the_snapshot_id_is_positive_and_names_the_content() {
    let digest = log::content_sha256([uuid::Uuid::now_v7()]);
    let id = snapshot_id_for(&digest).unwrap();
    assert!(id > 0);
    assert_eq!(id, snapshot_id_for(&digest).unwrap());
    assert_eq!(
        days_since_epoch(NaiveDate::from_ymd_opt(1970, 1, 2).unwrap()).unwrap(),
        1
    );
}

#[test]
fn every_exported_column_carries_its_field_id() {
    let schema = arrow_schema();
    for (i, field) in schema.fields().iter().enumerate() {
        assert_eq!(
            field.metadata().get(PARQUET_FIELD_ID_KEY),
            Some(&(i + 1).to_string()),
            "{}",
            field.name()
        );
    }
    let iceberg = crate::arrow_schema_to_iceberg(&schema).unwrap();
    assert!(iceberg.field_by_name("sent_at").is_some());
}
