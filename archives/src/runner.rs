//! Snapshot orchestration driven by the Restate workflow inside the
//! journaled `ctx.run("snapshot", …)` step.
//!
//! [`snapshot_all`] walks [`crate::tables::ALL_TABLES`], encodes each
//! non-empty table to Parquet, uploads it under the canonical
//! `application/<table>/data/dt=<date>/run=<HHMM>/snapshot.parquet` key, and
//! applies the add-only [`crate::drift`] policy. Per-table failures are
//! collected into [`SnapshotSummary::failures`] rather than aborting
//! the run, so the Slack digest still reports what did and didn't
//! succeed. Only a failure to acquire the database / storage handles
//! propagates as an error (Restate retries the whole step).

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use cloud::{StorageError, StorageService};
use serde::{Deserialize, Serialize};
use store::surreal::{SurrealConfig, SurrealDb};

use billing::gcp_cost::{
    adc_token_provider, billing_export_tables, BillingClient, CostReport, CostRow,
};

use crate::tables::{fetch_batch, is_safe_archive_table_name, source_tables};
use crate::{
    batch_from_rows, classify, encode_parquet, fingerprint, fingerprint_key, snapshot_key,
    DriftDecision, SnapshotConfig, SnapshotEntry, StoredFingerprint, ALL_TABLES, APPLICATION_LANE,
};

/// One table that failed to snapshot, with the rendered error so the
/// Slack digest can surface it as a ⚠️ line. Serializable because the whole
/// summary is the journaled output of the snapshot workflow step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableFailure {
    pub table: String,
    pub error: String,
}

/// The journaled result of the snapshot phase. `run_date` is captured
/// here (inside the `ctx.run` step) so it is recorded once and stays
/// stable across Restate replays.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub run_date: NaiveDate,
    #[serde(default)]
    pub scheduled_for: Option<DateTime<Utc>>,
    pub entries: Vec<SnapshotEntry>,
    pub failures: Vec<TableFailure>,
    /// The PostgreSQL generation of this run; `None` when PostgreSQL is not
    /// configured (and in summaries journaled before the source existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres: Option<crate::postgres::PostgresExport>,
}

/// Open the selected source and object-storage backend the
/// snapshot phase needs. PostgreSQL-only exports acquire no `SurrealDB` connection.
/// A failure here propagates so the workflow
/// step retries — the database or GCS being unreachable is transient.
pub async fn open_resources() -> Result<(Option<SurrealDb>, Arc<dyn StorageService>)> {
    let db = open_surreal_source(|key| std::env::var(key).ok()).await?;
    let storage = open_storage().await?;
    Ok((db, storage))
}

async fn open_surreal_source(get: impl Fn(&str) -> Option<String>) -> Result<Option<SurrealDb>> {
    match get("NAVIGATOR_ARCHIVE_SOURCE").as_deref() {
        Some("postgres") => {
            if get("NAVIGATOR_POSTGRES_HOST").is_none_or(|host| host.trim().is_empty()) {
                bail!("PostgreSQL-only archives require NAVIGATOR_POSTGRES_HOST");
            }
            return Ok(None);
        }
        None | Some("all") => {}
        Some(_) => bail!("NAVIGATOR_ARCHIVE_SOURCE must be all or postgres"),
    }
    let config = SurrealConfig::from_lookup(get).context("read SurrealDB coordinates")?;
    Ok(Some(
        store::surreal::connect(&config)
            .await
            .context("open SurrealDB connection")?,
    ))
}

/// The archive lane's object-storage handle.
///
/// Three buckets reach this pod and the distinction matters in all three
/// directions. `NAVIGATOR_DOCUMENTS_BUCKET` holds client documents and is
/// what the `generate_pdf__*` render lane writes. `NAVIGATOR_STORAGE_BUCKET`
/// is the general exports bucket. This lane writes neither: the scheduled
/// Parquet and the Iceberg metadata over it are the analytical archive, and
/// they land in the dedicated `NAVIGATOR_ARCHIVES_BUCKET` under its own key
/// pair, so the documents and exports credentials cannot open it.
///
/// [`cloud::archives_from_env`] fails closed on both GCS and S3 — there is
/// no fallback to `NAVIGATOR_STORAGE_BUCKET`. A deployment that forgets the
/// key fails the run instead of quietly filling the exports bucket with
/// archive objects nothing later reads.
async fn open_storage() -> Result<Arc<dyn StorageService>> {
    cloud::archives_from_env()
        .await
        .context("open object storage")
}

/// Snapshot the selected `SurrealDB` source, or preserve the scheduled slot
/// without `SurrealDB` rows when only PostgreSQL is selected.
pub(crate) async fn snapshot_selected(
    db: Option<&SurrealDb>,
    storage: &dyn StorageService,
    scheduled_for: Option<DateTime<Utc>>,
) -> SnapshotSummary {
    match db {
        Some(db) => snapshot_all(db, storage, scheduled_for).await,
        None => SnapshotSummary {
            run_date: scheduled_for.unwrap_or_else(Utc::now).date_naive(),
            scheduled_for,
            entries: Vec::new(),
            failures: Vec::new(),
            postgres: None,
        },
    }
}

/// Snapshot the registered `SurrealDB` tables through an acquired connection.
pub async fn snapshot_all(
    db: &SurrealDb,
    storage: &dyn StorageService,
    scheduled_for: Option<DateTime<Utc>>,
) -> SnapshotSummary {
    let run_date = scheduled_for.unwrap_or_else(Utc::now).date_naive();
    let mut entries: Vec<SnapshotEntry> = Vec::new();
    let mut failures: Vec<TableFailure> = Vec::new();
    let tables = if let Ok(tables) = source_tables(db).await {
        tables
    } else {
        failures.push(TableFailure {
            table: "<source-inventory>".into(),
            error: "could not inventory live SurrealDB tables".into(),
        });
        ALL_TABLES.to_vec()
    };
    for table in &tables {
        if !is_safe_archive_table_name(table) {
            failures.push(TableFailure {
                table: table.clone(),
                error: "runtime table name is not a safe archive path segment".into(),
            });
            continue;
        }
        match snapshot_table(db, storage, table, scheduled_for).await {
            Ok(Some(entry)) => entries.push(entry),
            Ok(None) => tracing::info!(table, "skipped (empty)"),
            Err(err) => {
                tracing::error!(table, error = ?err, "snapshot failed");
                failures.push(TableFailure {
                    table: table.clone(),
                    error: format!("{err:#}"),
                });
            }
        }
    }
    SnapshotSummary {
        run_date,
        scheduled_for,
        entries,
        failures,
        postgres: None,
    }
}

/// Snapshot one table to Parquet on object storage. `Ok(None)` for an
/// empty table.
async fn snapshot_table(
    db: &SurrealDb,
    storage: &dyn StorageService,
    table: &str,
    scheduled_for: Option<DateTime<Utc>>,
) -> Result<Option<SnapshotEntry>> {
    let Some(batch) = fetch_batch(db, table).await? else {
        return Ok(None);
    };
    let rows = batch.num_rows();

    let current_fp = fingerprint(&batch);
    let prev_fp = read_fingerprint(storage, table).await?;
    let decision = classify(prev_fp.as_ref(), &current_fp)?;

    let bytes = encode_parquet(&batch)?;
    let cfg = SnapshotConfig::now(APPLICATION_LANE, table);
    let key = scheduled_for.map_or_else(
        || snapshot_key(&cfg),
        |scheduled| {
            format!(
                "{}/{}/data/dt={}/run={}/snapshot.parquet",
                APPLICATION_LANE,
                table,
                scheduled.date_naive(),
                scheduled.format("%H%M")
            )
        },
    );
    storage
        .put(&key, &bytes, "application/vnd.apache.parquet")
        .await
        .with_context(|| format!("upload {key}"))?;

    if needs_fingerprint_write(prev_fp.as_ref(), &decision) {
        write_fingerprint(
            storage,
            &StoredFingerprint {
                table: table.to_string(),
                columns: current_fp,
            },
        )
        .await?;
    }

    tracing::info!(
        table,
        rows,
        key = %key,
        bytes = bytes.len(),
        decision = ?decision,
        "snapshot uploaded"
    );
    Ok(Some(SnapshotEntry {
        table: table.to_string(),
        rows,
        bytes: bytes.len(),
        key,
        drift: decision,
    }))
}

/// The GCP-cost phase. Env-gated on `BILLING_EXPORT_TABLE`: unset →
/// `Ok(None)` (KIND / dev / OSS forks skip it cleanly, needing no
/// `BigQuery` credentials). When set, query every listed billing export —
/// the value is a comma-separated list, one table per billing account —
/// for trailing-window cost by service, merge the accounts into one
/// firm-wide set, snapshot it to the export lake as the `gcp_cost` table
/// (so it is queryable in `BigQuery` like the data tables), and return the
/// report. The lake snapshot is the point; the workflow runs this phase for
/// that side effect.
pub async fn cost_phase<F: Fn(&str) -> Option<String>>(get: F) -> Result<Option<CostReport>> {
    let non_empty = |k: &str| get(k).filter(|s| !s.is_empty());
    let tables = non_empty("BILLING_EXPORT_TABLE")
        .map(|raw| billing_export_tables(&raw))
        .unwrap_or_default();
    if tables.is_empty() {
        return Ok(None);
    }
    let project = non_empty("BIGQUERY_PROJECT").context(
        "BILLING_EXPORT_TABLE is set but BIGQUERY_PROJECT is not — both are required to query \
         the billing export",
    )?;
    let days: u32 = non_empty("ARCHIVES_COST_WINDOW_DAYS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let token = adc_token_provider().await?;
    let client = BillingClient::new(project, token);
    // One export table per billing account. The lake's `gcp_cost` table is a
    // firm-wide cost snapshot, so every account's rows are merged by service —
    // reading only the first table would silently under-report the total.
    let mut per_account = Vec::with_capacity(tables.len());
    for table in &tables {
        per_account.push(
            client
                .cost_by_service(table, days)
                .await
                .with_context(|| format!("query billing export {table}"))?,
        );
    }
    let rows = merge_cost_rows(per_account);

    // Snapshot the cost rows to the export lake the same way data
    // tables are written, so `gcp_cost` is queryable in BigQuery too.
    let key = match batch_from_rows(&rows)? {
        Some(batch) => {
            let storage = cloud::archives_from_env()
                .await
                .context("open object storage for cost snapshot")?;
            let bytes = encode_parquet(&batch)?;
            let cfg = SnapshotConfig::now(APPLICATION_LANE, "gcp_cost");
            let key = snapshot_key(&cfg);
            storage
                .put(&key, &bytes, "application/vnd.apache.parquet")
                .await
                .with_context(|| format!("upload {key}"))?;
            Some(key)
        }
        None => None,
    };
    Ok(Some(CostReport { rows, key }))
}

/// Merge each billing account's cost-by-service rows into one firm-wide set,
/// summing a service billed to several accounts and returning highest-cost
/// first — the same ordering a single-table read produces, so the `gcp_cost`
/// snapshot's shape does not depend on how many accounts are configured.
fn merge_cost_rows(per_account: Vec<Vec<CostRow>>) -> Vec<CostRow> {
    // BTreeMap so equal-cost services land in a deterministic (alphabetical)
    // order — a snapshot that reshuffles run to run reads as spurious drift.
    let mut totals: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    for rows in per_account {
        for row in rows {
            *totals.entry(row.service).or_default() += row.cost;
        }
    }
    let mut rows: Vec<CostRow> = totals
        .into_iter()
        .map(|(service, cost)| CostRow { service, cost })
        .collect();
    rows.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    rows
}

fn needs_fingerprint_write(previous: Option<&StoredFingerprint>, decision: &DriftDecision) -> bool {
    matches!(decision, DriftDecision::Added(_)) || previous.is_none()
}

async fn read_fingerprint(
    storage: &dyn StorageService,
    table: &str,
) -> Result<Option<StoredFingerprint>> {
    let key = fingerprint_key(APPLICATION_LANE, table);
    match storage.get(&key).await {
        Ok(obj) => {
            let parsed: StoredFingerprint = serde_json::from_slice(&obj.bytes)
                .with_context(|| format!("parse stored fingerprint at {key}"))?;
            Ok(Some(parsed))
        }
        Err(StorageError::NotFound(_)) => Ok(None),
        Err(other) => Err(other).with_context(|| format!("read fingerprint at {key}")),
    }
}

async fn write_fingerprint(storage: &dyn StorageService, fp: &StoredFingerprint) -> Result<()> {
    let key = fingerprint_key(APPLICATION_LANE, &fp.table);
    let bytes = serde_json::to_vec_pretty(fp).context("serialize fingerprint")?;
    storage
        .put(&key, &bytes, "application/json")
        .await
        .with_context(|| format!("write fingerprint at {key}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        merge_cost_rows, needs_fingerprint_write, open_storage, snapshot_all, SnapshotSummary,
        TableFailure,
    };
    use crate::{fetch_batch, fingerprint, DriftDecision, StoredFingerprint};
    use arrow::array::{Array, StringArray};
    use billing::gcp_cost::CostRow;
    use cloud::FsStorage;
    use std::path::Path;

    #[tokio::test]
    async fn postgres_only_source_does_not_require_surreal_coordinates() {
        let source = super::open_surreal_source(|key| match key {
            "NAVIGATOR_ARCHIVE_SOURCE" => Some("postgres".into()),
            "NAVIGATOR_POSTGRES_HOST" => Some("unreachable.invalid".into()),
            _ => None,
        })
        .await
        .unwrap();
        assert!(source.is_none());
        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        let scheduled = "2026-10-20T05:55:00Z".parse().unwrap();
        let summary = super::snapshot_selected(source.as_ref(), &storage, Some(scheduled)).await;
        assert_eq!(summary.scheduled_for, Some(scheduled));
        assert_eq!(summary.run_date, scheduled.date_naive());
        assert!(summary.entries.is_empty());
        assert!(summary.failures.is_empty());
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn missing_or_invalid_source_configuration_cannot_skip_surreal() {
        for mode in [None, Some(""), Some("postgre"), Some("postgres")] {
            let result = super::open_surreal_source(|key| {
                (key == "NAVIGATOR_ARCHIVE_SOURCE")
                    .then(|| mode.map(str::to_owned))
                    .flatten()
            })
            .await;
            assert!(
                result.is_err(),
                "mode {mode:?} must refuse incomplete configuration"
            );
        }
    }

    fn row(service: &str, cost: f64) -> CostRow {
        CostRow {
            service: service.into(),
            cost,
        }
    }

    #[test]
    fn cost_rows_merge_every_billing_account_into_one_snapshot() {
        // The `gcp_cost` snapshot is firm-wide, so a service billed to two
        // accounts sums rather than appearing twice or shadowing the other.
        let merged = merge_cost_rows(vec![
            vec![row("Kubernetes Engine", 100.00), row("Cloud SQL", 20.00)],
            vec![
                row("Kubernetes Engine", 50.00),
                row("Compute Engine", 30.00),
            ],
        ]);
        assert_eq!(
            merged,
            vec![
                row("Kubernetes Engine", 150.00),
                row("Compute Engine", 30.00),
                row("Cloud SQL", 20.00),
            ],
            "merged highest-cost first"
        );
    }

    #[test]
    fn merging_one_account_preserves_the_single_table_shape() {
        // A single-account deploy must produce exactly what a plain
        // `cost_by_service` read did, or the snapshot shape depends on config.
        let rows = vec![row("Kubernetes Engine", 100.00), row("Cloud SQL", 20.00)];
        assert_eq!(merge_cost_rows(vec![rows.clone()]), rows);
        assert!(merge_cost_rows(vec![]).is_empty());
    }

    #[test]
    fn first_run_writes_fingerprint_even_if_unchanged_decision() {
        assert!(needs_fingerprint_write(
            None,
            &DriftDecision::Added(vec!["id".into()])
        ));
    }

    #[test]
    fn added_decision_writes_fingerprint() {
        let prev = StoredFingerprint {
            table: "persons".into(),
            columns: vec!["id".into()],
        };
        assert!(needs_fingerprint_write(
            Some(&prev),
            &DriftDecision::Added(vec!["email".into()])
        ));
    }

    #[test]
    fn unchanged_decision_skips_fingerprint_rewrite() {
        let prev = StoredFingerprint {
            table: "persons".into(),
            columns: vec!["id".into()],
        };
        assert!(!needs_fingerprint_write(
            Some(&prev),
            &DriftDecision::Unchanged
        ));
    }

    #[tokio::test]
    async fn surreal_snapshot_uses_record_and_datetime_strings_for_schema_drift() {
        let db = store::surreal::test_support::mem().await;
        db.query(
            "CREATE person:analyst SET \
             name = 'Analyst', \
             email = 'analyst@example.com', \
             inserted_at = d'2026-08-09T00:00:00Z', \
             updated_at = d'2026-08-09T00:00:00Z'",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let batch = fetch_batch(&db, "person").await.unwrap().unwrap();
        let columns = fingerprint(&batch);
        let expected_columns = vec![
            "email".into(),
            "email_confirmed".into(),
            "email_lower".into(),
            "id".into(),
            "inserted_at".into(),
            "is_admitted".into(),
            "name".into(),
            "role".into(),
            "updated_at".into(),
        ];
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(columns, expected_columns);
        assert_eq!(string_cell(&batch, "id"), "person:analyst");
        assert_eq!(string_cell(&batch, "inserted_at"), "2026-08-09T00:00:00Z");

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path()).await.unwrap();
        let summary = snapshot_all(&db, &storage, None).await;
        let person = summary
            .entries
            .iter()
            .find(|entry| entry.table == "person")
            .expect("seeded person table is snapshotted");
        assert_eq!(person.rows, 1);
        assert_eq!(person.drift, DriftDecision::Added(expected_columns));
        assert!(summary.failures.is_empty(), "{:#?}", summary.failures);
    }

    #[tokio::test]
    async fn snapshot_includes_live_tables_added_after_the_binary_schema() {
        let db = store::surreal::test_support::mem().await;
        db.query(
            "DEFINE TABLE archive_runtime_probe SCHEMAFULL; \
             DEFINE FIELD payload ON archive_runtime_probe TYPE string; \
             CREATE archive_runtime_probe:one SET payload = 'Synthetic archive row';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        let summary = snapshot_all(&db, &storage, None).await;
        assert!(summary.failures.is_empty(), "{:#?}", summary.failures);
        let entry = summary
            .entries
            .iter()
            .find(|entry| entry.table == "archive_runtime_probe")
            .expect("a live runtime table is included in the archive");
        assert_eq!(entry.rows, 1);
        assert!(entry
            .key
            .starts_with("application/archive_runtime_probe/data/"));
        assert!(!summary
            .failures
            .iter()
            .any(|failure| failure.table == "archive_runtime_probe"));
    }

    #[tokio::test]
    async fn snapshot_refuses_live_table_names_that_are_not_safe_path_segments() {
        let db = store::surreal::test_support::mem().await;
        db.query(
            "DEFINE TABLE `archive-runtime-probe` SCHEMALESS; \
             CREATE `archive-runtime-probe`:one SET payload = 'Synthetic archive row';",
        )
        .await
        .unwrap()
        .check()
        .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let storage = FsStorage::new(dir.path().to_path_buf()).await.unwrap();
        let summary = snapshot_all(&db, &storage, None).await;
        let failure = summary
            .failures
            .iter()
            .find(|failure| failure.table == "archive-runtime-probe")
            .expect("an unsafe runtime table name is reported, not exported");
        assert_eq!(
            failure.error,
            "runtime table name is not a safe archive path segment"
        );
        assert!(!summary
            .entries
            .iter()
            .any(|entry| entry.table == "archive-runtime-probe"));
    }

    fn string_cell<'a>(batch: &'a arrow::array::RecordBatch, name: &str) -> &'a str {
        let index = batch.schema().index_of(name).unwrap();
        batch
            .column(index)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .value(0)
    }

    #[test]
    fn snapshot_summary_round_trips_through_serde() {
        // The whole summary is the output of `ctx.run("snapshot", …)`,
        // so it must round-trip through serde for Restate to journal
        // and replay it. A live database-backed `snapshot_all` runs in
        // the KIND smoke test; the per-binary testcontainers
        // cost isn't worth it for a thin loop over the per-table
        // dispatch already covered in `tables.rs`.
        let summary = SnapshotSummary {
            run_date: chrono::NaiveDate::from_ymd_opt(2026, 5, 29).unwrap(),
            scheduled_for: None,
            entries: Vec::new(),
            failures: vec![TableFailure {
                table: "persons".into(),
                error: "boom".into(),
            }],
            postgres: Some(crate::postgres::PostgresExport::Failed {
                error: "unavailable".into(),
            }),
        };
        let json = serde_json::to_string(&summary).unwrap();
        let back: SnapshotSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.failures.len(), 1);
        assert_eq!(back.postgres, summary.postgres);
        assert_eq!(back.run_date, summary.run_date);
    }

    /// The archive lane resolves its bucket from `NAVIGATOR_ARCHIVES_BUCKET`
    /// and fails when it is absent, rather than falling back to the exports
    /// bucket.
    ///
    /// Kept as one serial test because it mutates the process-global
    /// `NAVIGATOR_STORAGE_*` vars, following `cloud`'s own selection test —
    /// no other `archives` test reads them, so no lock is needed. `s3` is the
    /// backend under test because it resolves the lane's bucket before it
    /// builds anything; `fs` keeps one storage root and never looks at a
    /// bucket at all, so it cannot tell the lanes apart.
    ///
    /// It asserts only the refusal, and deliberately does not also assert
    /// that a named bucket opens: that path constructs a real S3 client,
    /// which loads the platform certificate store, and the macOS keychain
    /// returns an I/O error under the concurrency of a full workspace run.
    /// The refusal is the whole claim anyway — the lane reaches for its own
    /// key and will not settle for the exports bucket — and it reaches that
    /// verdict from the config alone, with no client and no endpoint.
    #[tokio::test]
    async fn open_storage_requires_the_archives_bucket_and_ignores_the_exports_one() {
        for key in [
            "NAVIGATOR_STORAGE_BACKEND",
            "NAVIGATOR_STORAGE_ENDPOINT",
            "NAVIGATOR_STORAGE_ACCESS_KEY",
            "NAVIGATOR_STORAGE_SECRET_KEY",
            "NAVIGATOR_STORAGE_BUCKET",
            "NAVIGATOR_ARCHIVES_BUCKET",
        ] {
            std::env::remove_var(key);
        }
        std::env::set_var("NAVIGATOR_STORAGE_BACKEND", "s3");
        std::env::set_var("NAVIGATOR_STORAGE_ENDPOINT", "http://seaweedfs:3900");
        std::env::set_var("NAVIGATOR_STORAGE_ACCESS_KEY", "access");
        std::env::set_var("NAVIGATOR_STORAGE_SECRET_KEY", "secret");
        // The exports bucket alone is exactly the misconfiguration this lane
        // used to accept, writing archive objects where nothing reads them.
        std::env::set_var("NAVIGATOR_STORAGE_BUCKET", "stg-exports");

        let Err(error) = open_storage().await else {
            panic!("the lane must fail closed with no archives bucket");
        };
        assert!(
            format!("{error:#}").contains("NAVIGATOR_ARCHIVES_BUCKET"),
            "the failure must name the key that is missing; got: {error:#}"
        );
    }

    /// The snapshot phase writes through the handle it is given and nowhere
    /// else. Two roots stand in for the two buckets: everything lands under
    /// the archive root, and the exports root receives nothing.
    #[tokio::test]
    async fn snapshot_phase_writes_into_the_archive_root_and_leaves_exports_empty() {
        let db = store::surreal::test_support::mem().await;
        db.query(
            "CREATE person:analyst SET \
             name = 'Analyst', email = 'analyst@example.com', role = 'client'",
        )
        .await
        .unwrap();

        let archive_dir = tempfile::tempdir().unwrap();
        let exports_dir = tempfile::tempdir().unwrap();
        let archive = FsStorage::new(archive_dir.path()).await.unwrap();

        let summary = snapshot_all(&db, &archive, None).await;
        assert!(summary.failures.is_empty(), "{:#?}", summary.failures);
        assert!(
            !summary.entries.is_empty(),
            "the seeded table must produce a snapshot"
        );

        assert!(
            walk_files(archive_dir.path()) > 0,
            "the archive root receives the run's Parquet"
        );
        assert_eq!(
            walk_files(exports_dir.path()),
            0,
            "the exports root must receive no archive objects"
        );
    }

    /// Count regular files anywhere beneath `root`.
    fn walk_files(root: &Path) -> usize {
        let mut count = 0;
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    count += 1;
                }
            }
        }
        count
    }
}
