//! The archives crate never deletes a backup.
//!
//! Two independent proofs. A source-level guard asserts no non-test code
//! under `archives/src` contains a delete call site — the same technique
//! as `no_unset_bucket_sentinel.rs`, where a behavioural test per path
//! cannot prove that *no* path exists. And a behavioural run drives a
//! full snapshot plus Iceberg promotion through a storage double that
//! panics on `delete`: an undisturbed run is the zero-deletes proof.
//! The paired PostgreSQL drill runs its export leg through the same
//! kind of panicking double (`coverage_postgres.rs`).

use std::fs;
use std::path::{Path, PathBuf};

/// The call the guard forbids, assembled at runtime so this file does
/// not itself contain the phrase it forbids.
fn forbidden() -> String {
    format!(".{}(", "delete")
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            found.push(path);
        }
    }
}

/// Line numbers of `forbidden()` occurrences outside test-gated code.
///
/// This codebase keeps its `#[cfg(test)]` modules last, so everything
/// from the file's final `#[cfg(test)]` marker down compiles only under
/// `cargo test` — a test double implementing delete there is a mock, not
/// a production deletion path.
fn non_test_delete_call_sites(body: &str) -> Vec<usize> {
    let gate = body
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim() == "#[cfg(test)]")
        .map(|(index, _)| index)
        .max()
        .unwrap_or(usize::MAX);
    let needle = forbidden();
    body.lines()
        .enumerate()
        .filter(|(index, line)| *index < gate && line.contains(&needle))
        .map(|(index, _)| index + 1)
        .collect()
}

#[test]
fn no_non_test_code_under_archives_deletes() {
    let root = crate_root();
    let mut sources = Vec::new();
    rust_sources(&root.join("src"), &mut sources);
    assert!(!sources.is_empty(), "the crate must have Rust sources");

    let offenders: Vec<String> = sources
        .iter()
        .filter_map(|path| {
            let body = fs::read_to_string(path).ok()?;
            let lines = non_test_delete_call_sites(&body);
            if lines.is_empty() {
                None
            } else {
                Some(format!(
                    "{}: lines {}",
                    path.strip_prefix(&root).unwrap_or(path).to_string_lossy(),
                    lines
                        .iter()
                        .map(|l| l.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "the archive writer must never delete a backup — create-only \
         writes, an IAM grant without delete, and a locked retention \
         policy hold that guarantee; these files contain a delete call \
         outside a test module:\n  {}",
        offenders.join("\n  ")
    );
}

/// A full archive run — snapshot plus Iceberg promotion — through a
/// retention-locked double whose `delete` panics: no panic means the
/// run performed zero deletes end to end.
#[tokio::test]
async fn a_full_archive_run_performs_zero_deletes() {
    use archives::test_support::CollisionStorage;
    use archives::{author_iceberg_for_prefix, encode_parquet, PromotionRequest, APPLICATION_LANE};
    use arrow::array::{RecordBatch, StringArray};
    use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
    use cloud::StorageService;

    let db = store::surreal::test_support::mem().await;
    db.query(
        "CREATE person:analyst SET \
         name = 'Analyst', email = 'analyst@example.com', role = 'client'",
    )
    .await
    .unwrap();

    // The snapshot leg: every application table through the panicking
    // double. `CollisionStorage` panics on `delete` — an undisturbed run
    // is the proof.
    let storage = CollisionStorage::locked();
    let summary = archives::snapshot_all(&db, storage.as_ref(), None).await;
    assert!(summary.failures.is_empty(), "{:#?}", summary.failures);

    // The promotion leg over a real day of Parquet.
    let run_date = chrono::NaiveDate::from_ymd_opt(2026, 6, 14).unwrap();
    let schema = ArrowSchema::new(vec![Field::new("id", DataType::Utf8, false)]);
    let parquet = encode_parquet(
        &RecordBatch::try_new(
            std::sync::Arc::new(schema),
            vec![std::sync::Arc::new(StringArray::from(vec!["synthetic"]))],
        )
        .unwrap(),
    )
    .unwrap();
    let data_key = format!("{APPLICATION_LANE}/person/data/dt={run_date}/part-1.parquet");
    storage
        .put_immutable(&data_key, &parquet, "application/vnd.apache.parquet")
        .await
        .unwrap();
    let authored = author_iceberg_for_prefix(
        storage.as_ref(),
        PromotionRequest {
            lane: APPLICATION_LANE,
            table: "person",
            location_base: "gs://synthetic-exports",
            run_date,
            data_key: None,
            snapshot_id: 9_001,
            timestamp_ms: 1_780_000_000_000,
            expire_before_ms: None,
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(authored.version, 1);
}
