//! Create-only writes end to end.
//!
//! The runner and iceberg unit tests each cover one half of the write
//! path; this walkthrough drives the public crate API through a
//! retention-locked storage double: two Iceberg promotions chained over
//! attempt-unique Parquet uploads, proving v1 metadata is never rewritten,
//! the mutable version hint is never written, and both metadata versions
//! persist alongside every data file.

use archives::test_support::CollisionStorage;
use archives::{author_iceberg_for_prefix, encode_parquet, PromotionRequest, APPLICATION_LANE};
use arrow::array::{RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use chrono::NaiveDate;
use cloud::StorageService;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chained_promotions_through_a_locked_bucket_replace_nothing() {
    let storage = CollisionStorage::locked();
    let run_date = NaiveDate::from_ymd_opt(2026, 6, 14).unwrap();
    let schema = Arc::new(ArrowSchema::new(vec![Field::new(
        "id",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        Arc::clone(&schema),
        vec![Arc::new(StringArray::from(vec!["synthetic"]))],
    )
    .unwrap();
    let parquet = encode_parquet(&batch).unwrap();

    // Two same-day snapshots of one table: on a retention-locked bucket
    // each lands under its own attempt-unique key via put_immutable —
    // a rerun never replaces the first snapshot's bytes.
    for attempt in [1, 2] {
        let key = format!(
            "{APPLICATION_LANE}/person/data/dt={run_date}/run=0555/attempt={attempt}/snapshot.parquet"
        );
        storage
            .put_immutable(&key, &parquet, "application/vnd.apache.parquet")
            .await
            .unwrap();
    }

    let request = PromotionRequest {
        lane: APPLICATION_LANE,
        table: "person",
        location_base: "gs://synthetic-exports",
        run_date,
        data_key: None,
        snapshot_id: 7001,
        timestamp_ms: 1_780_000_000_000,
        expire_before_ms: None,
    };
    let first = author_iceberg_for_prefix(storage.as_ref(), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.version, 1);

    let mut second = request;
    second.snapshot_id = 7002;
    let second = author_iceberg_for_prefix(storage.as_ref(), second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.version, 2, "the second promotion chains onto v1");

    // v1 metadata bytes are unchanged; no version hint exists on the
    // locked lane; every generation's metadata persists.
    let v1_key = format!("{APPLICATION_LANE}/person/metadata/v1.metadata.json");
    let v1_first = first
        .files
        .iter()
        .find(|(name, _)| *name == "metadata/v1.metadata.json")
        .map(|(_, bytes)| bytes.clone())
        .unwrap();
    assert_eq!(
        storage.get(&v1_key).await.unwrap().bytes,
        v1_first,
        "v1 metadata was rewritten"
    );
    assert!(storage.contains(&v1_key));
    assert!(storage.contains(&format!(
        "{APPLICATION_LANE}/person/metadata/v2.metadata.json"
    )));
    assert!(
        !storage.contains(&format!(
            "{APPLICATION_LANE}/person/metadata/version-hint.text"
        )),
        "a locked policy rejects replacing the mutable hint"
    );
}
