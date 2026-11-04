//! The Sent Email Export: the daily DBOS workflow that copies each closed UTC
//! day of PostgreSQL's append-only `sent_email` trail to an Apache Iceberg
//! table, then purges rows older than a year the export has confirmed.
//!
//! The table lives under [`TABLE_PREFIX`] in the archive bucket, apart from the
//! full-table `postgres/navigator/sent_email` coverage lineage, and is
//! **partitioned by day** (`day(sent_at)`): each UTC day is one Parquet data
//! file, `data/sent_at_day=<date>/part-<sha256>.parquet`, named by the digest
//! of the day's row ids, listed in its own manifest,
//! `metadata/day-<date>-<sha256>-m0.avro`. Every snapshot's manifest list
//! carries the earlier days forward, so the current snapshot holds the whole
//! trail even after PostgreSQL has purged a day.
//!
//! A run is idempotent. A day whose ledger row (`sent_email_export`) already
//! has the day's digest is skipped. A day that gained a late row is written
//! again and its earlier manifest dropped from the new snapshot (an Iceberg
//! overwrite; the prior snapshot still reads the old file). A crash between
//! the Iceberg commit and the ledger write finds the day's manifest already in
//! the current snapshot on retry and records the ledger without a second
//! commit. Metadata objects are written before `version-hint.text`, which
//! moves last.
//!
//! After the export, [`store::sent_emails::postgres::purge`] deletes the rows
//! of each exported day more than a year old whose count and digest still
//! match the ledger. Journals carry dates and counts, never message content.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use arrow::array::{ArrayRef, RecordBatch, StringArray, TimestampMicrosecondArray};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema, TimeUnit};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use cloud::{StorageError, StorageService};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use iceberg::io::FileIO;
use iceberg::spec::{
    DataContentType, DataFileBuilder, DataFileFormat, FormatVersion, Literal, ManifestFile,
    ManifestList, ManifestListWriter, ManifestWriterBuilder, Operation, PartitionSpec, Snapshot,
    SnapshotReference, SnapshotRetention, SortOrder, Struct, Summary, TableMetadata,
    TableMetadataBuilder, Transform, MAIN_BRANCH,
};
use serde::{Deserialize, Serialize};
use store::postgres::PostgresDb;
use store::sent_emails::postgres::{self as log, DayExport, DayPurge};
use store::sent_emails::SentEmail;
use workflows::dbos_engine::daily::{self, LateStart};

/// The Iceberg table's prefix in the archive bucket.
pub const TABLE_PREFIX: &str = "email/sent_email";
/// Independent opt-in for the schedule. Only literal `true` enables it.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED";
const WORKFLOW: &str = "SentEmailExport";
const PARQUET_FIELD_ID_KEY: &str = "PARQUET:field_id";
const PARTITION_FIELD: &str = "sent_at_day";

pub type SentEmailExportWorkflow = WorkflowRef<(), RunReport, ExportError>;

/// What one run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunReport {
    /// `(day, outcome)` for every day the run exported or found unchanged.
    pub exported: Vec<(NaiveDate, String)>,
    /// `(day, outcome)` for every day the purge considered.
    pub purged: Vec<(NaiveDate, String)>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ExportError {
    #[error("the days pending export could not be read")]
    Pending,
    #[error("a day could not be exported")]
    Export,
    #[error("the retention purge failed")]
    Purge,
}

/// What exporting one day did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayOutcome {
    /// The ledger already records the day's digest.
    Unchanged { rows: u64 },
    /// A new snapshot holds the day.
    Committed { rows: u64, snapshot_id: i64 },
    /// The current snapshot already held the day; only the ledger was written.
    Recorded { rows: u64 },
}

impl DayOutcome {
    fn line(&self) -> String {
        match self {
            Self::Unchanged { rows } => format!("unchanged ({rows} rows)"),
            Self::Committed { rows, snapshot_id } => {
                format!("exported {rows} rows in snapshot {snapshot_id}")
            }
            Self::Recorded { rows } => format!("recorded {rows} rows already in the table"),
        }
    }
}

/// Whether this schedule is enabled. Only literal `true` opts in.
///
/// # Errors
///
/// Any other non-empty value, so a typo cannot silently leave it off.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED must be true or false"),
    }
}

/// The Arrow shape of one exported row. Field ids are explicit and stable,
/// so the Parquet files and the Iceberg schema name the same columns.
#[must_use]
pub fn arrow_schema() -> ArrowSchema {
    let text = |name: &str, nullable: bool| Field::new(name, DataType::Utf8, nullable);
    let time = |name: &str| {
        Field::new(
            name,
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        )
    };
    let fields = [
        text("id", false),
        text("recipient", false),
        text("subject", false),
        text("sender", false),
        text("project_id", true),
        text("person_id", true),
        text("template_slug", true),
        text("body", false),
        text("outcome", false),
        text("sg_message_id", true),
        time("sent_at"),
        time("inserted_at"),
    ];
    ArrowSchema::new(
        fields
            .into_iter()
            .enumerate()
            .map(|(i, field)| {
                field.with_metadata(HashMap::from([(
                    PARQUET_FIELD_ID_KEY.to_string(),
                    (i + 1).to_string(),
                )]))
            })
            .collect::<Vec<_>>(),
    )
}

/// One day's rows as a record batch in [`arrow_schema`]'s shape.
///
/// # Errors
///
/// The batch does not match the schema (a programming error).
pub fn batch(rows: &[SentEmail]) -> Result<RecordBatch> {
    let text = |f: fn(&SentEmail) -> Option<String>| -> ArrayRef {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>()))
    };
    let time = |f: fn(&SentEmail) -> DateTime<Utc>| -> ArrayRef {
        Arc::new(
            TimestampMicrosecondArray::from(
                rows.iter()
                    .map(|row| f(row).timestamp_micros())
                    .collect::<Vec<_>>(),
            )
            .with_timezone("UTC"),
        )
    };
    let columns: Vec<ArrayRef> = vec![
        text(|r| Some(r.id.to_string())),
        text(|r| Some(r.recipient.clone())),
        text(|r| Some(r.subject.clone())),
        text(|r| Some(r.sender.clone())),
        text(|r| r.project_id.map(|id| id.to_string())),
        text(|r| r.person_id.map(|id| id.to_string())),
        text(|r| r.template_slug.clone()),
        text(|r| Some(r.body.clone())),
        text(|r| Some(r.outcome.clone())),
        text(|r| r.sg_message_id.clone()),
        time(|r| r.sent_at),
        time(|r| r.inserted_at),
    ];
    RecordBatch::try_new(Arc::new(arrow_schema()), columns).context("build sent_email batch")
}

fn days_since_epoch(day: NaiveDate) -> Result<i32> {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).context("epoch")?;
    i32::try_from((day - epoch).num_days()).context("day out of range")
}

/// A snapshot id unique to one day's content.
fn snapshot_id_for(digest: &str) -> Result<i64> {
    let prefix = digest.get(..15).context("digest too short")?;
    i64::from_str_radix(prefix, 16).context("digest is not hex")
}

fn manifest_name(day: NaiveDate, digest: &str) -> String {
    format!("day-{day}-{digest}-m0.avro")
}

fn day_manifest_marker(day: NaiveDate) -> String {
    format!("/metadata/day-{day}-")
}

/// The storage key of an absolute object URI under `location_base`.
fn key_of<'a>(location_base: &str, uri: &'a str) -> Result<&'a str> {
    uri.strip_prefix(location_base)
        .and_then(|rest| rest.strip_prefix('/'))
        .with_context(|| format!("{uri} is outside the archive location"))
}

/// The table as it stands: its current metadata, version, and the manifests
/// its current snapshot lists.
struct Current {
    metadata: TableMetadata,
    version: u64,
    manifests: Vec<ManifestFile>,
}

async fn current(storage: &dyn StorageService, location_base: &str) -> Result<Option<Current>> {
    let hint = match storage
        .get(&format!("{TABLE_PREFIX}/metadata/version-hint.text"))
        .await
    {
        Ok(object) => object,
        Err(StorageError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let version: u64 = String::from_utf8_lossy(&hint.bytes)
        .trim()
        .parse()
        .context("parse version-hint.text")?;
    let bytes = storage
        .get(&format!("{TABLE_PREFIX}/metadata/v{version}.metadata.json"))
        .await?
        .bytes;
    let metadata: TableMetadata =
        serde_json::from_slice(&bytes).context("decode sent_email table metadata")?;
    let manifests = match metadata.current_snapshot() {
        Some(snapshot) => {
            let list = storage
                .get(key_of(location_base, snapshot.manifest_list())?)
                .await?
                .bytes;
            ManifestList::parse_with_version(&list, FormatVersion::V2)
                .context("decode manifest list")?
                .consume_entries()
                .into_iter()
                .collect()
        }
        None => Vec::new(),
    };
    Ok(Some(Current {
        metadata,
        version,
        manifests,
    }))
}

/// A fresh table: the Arrow-derived schema, partitioned by `day(sent_at)`.
fn new_table(table_location: &str) -> Result<TableMetadata> {
    let schema = crate::arrow_schema_to_iceberg(&arrow_schema())?;
    let spec = PartitionSpec::builder(schema.clone())
        .with_spec_id(0)
        .add_partition_field("sent_at", PARTITION_FIELD, Transform::Day)
        .context("partition by day(sent_at)")?
        .build()
        .context("build partition spec")?;
    Ok(TableMetadataBuilder::new(
        schema,
        spec.into_unbound(),
        SortOrder::unsorted_order(),
        table_location.to_string(),
        FormatVersion::V2,
        HashMap::new(),
    )
    .context("new sent_email table metadata")?
    .build()
    .context("build sent_email table metadata")?
    .metadata)
}

/// Everything one day's commit needs.
struct DayCommit<'a> {
    table_location: &'a str,
    day: NaiveDate,
    digest: &'a str,
    data_file: &'a str,
    rows: u64,
    file_size: u64,
    timestamp_ms: i64,
}

/// Author a snapshot that lists every earlier day's manifest except `day`'s,
/// plus `day`'s new one. Returns `(relative path, bytes)` objects, the
/// version hint last, and the new version.
#[expect(
    clippy::too_many_lines,
    reason = "one day's commit coordinates its manifest, manifest list, snapshot, and metadata pointer"
)]
async fn author_day(
    commit: &DayCommit<'_>,
    current: Option<&Current>,
) -> Result<(Vec<(String, Vec<u8>)>, u64, i64)> {
    let base = match current {
        Some(current) => current.metadata.clone(),
        None => new_table(commit.table_location)?,
    };
    let expected = crate::arrow_schema_to_iceberg(&arrow_schema())?;
    if base.current_schema().as_struct() != expected.as_struct() {
        bail!("the sent_email Iceberg schema changed; a new column needs an explicit evolution");
    }
    let schema = base.current_schema().clone();
    let spec: PartitionSpec = (**base.default_partition_spec()).clone();
    let snapshot_id = snapshot_id_for(commit.digest)?;
    let sequence_number = base.last_sequence_number() + 1;
    let io = FileIO::new_with_memory();

    let manifest = manifest_name(commit.day, commit.digest);
    let manifest_uri = format!("{}/metadata/{manifest}", commit.table_location);
    let mut writer = ManifestWriterBuilder::new(
        io.new_output(&manifest_uri)?,
        Some(snapshot_id),
        schema,
        spec,
    )
    .build_v2_data();
    writer
        .add_file(
            DataFileBuilder::default()
                .content(DataContentType::Data)
                .file_path(commit.data_file.to_string())
                .file_format(DataFileFormat::Parquet)
                .partition(Struct::from_iter([Some(Literal::date(days_since_epoch(
                    commit.day,
                )?))]))
                .record_count(commit.rows)
                .file_size_in_bytes(commit.file_size)
                .build()
                .context("build data file")?,
            sequence_number,
        )
        .context("add data file")?;
    let manifest_file = writer
        .write_manifest_file()
        .await
        .context("write manifest")?;
    let manifest_bytes = io.new_input(&manifest_uri)?.read().await?.to_vec();

    let marker = day_manifest_marker(commit.day);
    let earlier: Vec<ManifestFile> = current.map_or_else(Vec::new, |c| c.manifests.clone());
    let replaced = earlier.iter().any(|m| m.manifest_path.contains(&marker));
    let kept = earlier
        .into_iter()
        .filter(|m| !m.manifest_path.contains(&marker));

    let parent = base.current_snapshot_id();
    let list_name = format!("snap-{snapshot_id}-{}.avro", commit.digest);
    let list_uri = format!("{}/metadata/{list_name}", commit.table_location);
    let mut list = ManifestListWriter::v2(
        io.new_output(&list_uri)?.writer().await?,
        snapshot_id,
        parent,
        sequence_number,
    );
    list.add_manifests(kept.chain(std::iter::once(manifest_file)))
        .context("add manifests to list")?;
    list.close().await.context("write manifest list")?;
    let list_bytes = io.new_input(&list_uri)?.read().await?.to_vec();

    let snapshot = Snapshot::builder()
        .with_snapshot_id(snapshot_id)
        .with_parent_snapshot_id(parent)
        .with_sequence_number(sequence_number)
        .with_timestamp_ms(commit.timestamp_ms)
        .with_manifest_list(list_uri)
        .with_summary(Summary {
            operation: if replaced {
                Operation::Overwrite
            } else {
                Operation::Append
            },
            additional_properties: HashMap::new(),
        })
        .with_schema_id(base.current_schema_id())
        .build();
    let version = current.map_or(1, |c| c.version + 1);
    let prior_location = current.map(|c| {
        format!(
            "{}/metadata/v{}.metadata.json",
            commit.table_location, c.version
        )
    });
    let metadata = TableMetadataBuilder::new_from_metadata(base, prior_location)
        .add_snapshot(snapshot)
        .context("add snapshot")?
        .set_ref(
            MAIN_BRANCH,
            SnapshotReference::new(snapshot_id, SnapshotRetention::branch(None, None, None)),
        )
        .context("set main")?
        .build()
        .context("build metadata")?
        .metadata;
    Ok((
        vec![
            (format!("metadata/{manifest}"), manifest_bytes),
            (format!("metadata/{list_name}"), list_bytes),
            (
                format!("metadata/v{version}.metadata.json"),
                serde_json::to_vec_pretty(&metadata)?,
            ),
            (
                "metadata/version-hint.text".to_string(),
                version.to_string().into_bytes(),
            ),
        ],
        version,
        snapshot_id,
    ))
}

fn content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("avro") => "application/avro",
        Some("json") => "application/json",
        Some("parquet") => "application/vnd.apache.parquet",
        _ => "text/plain",
    }
}

/// Export one closed UTC day: write its data file, commit it to the Iceberg
/// table unless the current snapshot already holds it, and record the ledger.
///
/// # Errors
///
/// PostgreSQL, object storage, or Iceberg authoring failed; the ledger is
/// written only after the commit, so a retry repeats safely.
pub async fn export_day(
    pg: &PostgresDb,
    storage: &dyn StorageService,
    location_base: &str,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> Result<DayOutcome> {
    let rows = log::day_rows(pg, day).await?;
    let count = rows.len() as u64;
    let digest = log::content_sha256(rows.iter().map(|row| row.id));
    if log::export_for(pg, day)
        .await?
        .is_some_and(|ledger| ledger.content_sha256 == digest)
    {
        return Ok(DayOutcome::Unchanged { rows: count });
    }
    let table_location = format!("{location_base}/{TABLE_PREFIX}");
    let data_rel = format!("data/{PARTITION_FIELD}={day}/part-{digest}.parquet");
    let data_file = format!("{table_location}/{data_rel}");
    let current = current(storage, location_base).await?;
    let manifest = manifest_name(day, &digest);
    let committed = current.as_ref().and_then(|c| {
        c.manifests
            .iter()
            .any(|m| m.manifest_path.ends_with(&manifest))
            .then(|| (c.version, c.metadata.current_snapshot_id()))
    });
    let (outcome, version, snapshot_id) = if let Some((version, Some(snapshot_id))) = committed {
        (DayOutcome::Recorded { rows: count }, version, snapshot_id)
    } else {
        let bytes = crate::encode_parquet(&batch(&rows)?)?;
        storage
            .put(
                &format!("{TABLE_PREFIX}/{data_rel}"),
                &bytes,
                content_type(&data_rel),
            )
            .await?;
        let (files, version, snapshot_id) = author_day(
            &DayCommit {
                table_location: &table_location,
                day,
                digest: &digest,
                data_file: &data_file,
                rows: count,
                file_size: bytes.len() as u64,
                timestamp_ms: now.timestamp_millis(),
            },
            current.as_ref(),
        )
        .await?;
        for (path, bytes) in &files {
            storage
                .put(&format!("{TABLE_PREFIX}/{path}"), bytes, content_type(path))
                .await?;
        }
        (
            DayOutcome::Committed {
                rows: count,
                snapshot_id,
            },
            version,
            snapshot_id,
        )
    };
    log::record_export(
        pg,
        &DayExport {
            day,
            row_count: count,
            content_sha256: digest,
            data_file,
            snapshot_id,
            metadata_version: version,
            exported_at: now,
            purged_at: None,
        },
    )
    .await?;
    Ok(outcome)
}

/// The run a schedule performs, behind a trait so tests can drive it.
#[async_trait::async_trait]
pub trait ExportSource: Send + Sync {
    async fn pending(&self, now: DateTime<Utc>) -> Result<Vec<NaiveDate>, String>;
    async fn export(&self, day: NaiveDate, now: DateTime<Utc>) -> Result<String, String>;
    async fn purge(&self, now: DateTime<Utc>) -> Result<Vec<(NaiveDate, String)>, String>;
}

/// The deployed source: the application's PostgreSQL login and the archive
/// bucket.
pub struct StoreExportSource {
    pub pg: PostgresDb,
    pub storage: Arc<dyn StorageService>,
    /// `gs://<archive-bucket>`.
    pub location_base: String,
}

#[async_trait::async_trait]
impl ExportSource for StoreExportSource {
    async fn pending(&self, now: DateTime<Utc>) -> Result<Vec<NaiveDate>, String> {
        log::pending_days(&self.pg, now.date_naive())
            .await
            .map_err(|error| error.to_string())
    }

    async fn export(&self, day: NaiveDate, now: DateTime<Utc>) -> Result<String, String> {
        export_day(
            &self.pg,
            self.storage.as_ref(),
            &self.location_base,
            day,
            now,
        )
        .await
        .map(|outcome| outcome.line())
        .map_err(|error| format!("{error:#}"))
    }

    async fn purge(&self, now: DateTime<Utc>) -> Result<Vec<(NaiveDate, String)>, String> {
        Ok(log::purge(&self.pg, now)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|(day, outcome)| {
                let line = match outcome {
                    DayPurge::Purged { deleted, retained } => {
                        format!("purged {deleted} rows, kept {retained} as service evidence")
                    }
                    DayPurge::NotExported => "skipped: rows not yet exported".to_string(),
                };
                (day, line)
            })
            .collect())
    }
}

impl StoreExportSource {
    /// Open the application's PostgreSQL pool and the archive bucket.
    ///
    /// # Errors
    ///
    /// PostgreSQL or `NAVIGATOR_ARCHIVES_BUCKET` is not configured.
    pub async fn from_env() -> Result<Self> {
        let bucket = std::env::var("NAVIGATOR_ARCHIVES_BUCKET")
            .ok()
            .filter(|bucket| !bucket.trim().is_empty())
            .context("the Sent Email Export requires NAVIGATOR_ARCHIVES_BUCKET")?;
        Ok(Self {
            pg: PostgresDb::from_env()
                .await
                .context("connect PostgreSQL for the Sent Email Export")?,
            storage: cloud::archives_from_env()
                .await
                .context("open the archive bucket")?,
            location_base: format!("gs://{bucket}"),
        })
    }
}

/// 02:41 UTC: after midnight closes the day, before the 05:55 Archives run.
#[must_use]
pub fn slot_time() -> NaiveTime {
    NaiveTime::from_hms_opt(2, 41, 0).unwrap_or(NaiveTime::MIN)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%d"))
}

/// The slot a workflow id names, so a replayed run judges "today" and the
/// retention cutoff at the same instant however late it recovers.
fn slot_of(workflow_id: &str) -> Option<DateTime<Utc>> {
    let date = workflow_id.strip_prefix(&format!("{WORKFLOW}-"))?;
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(daily::slot(
        date.and_time(NaiveTime::MIN).and_utc(),
        slot_time(),
    ))
}

/// Register the workflow: one step lists the pending days, one step per day
/// exports it, and a last step purges. A retry joins the recorded steps.
///
/// # Errors
///
/// DBOS refuses the registration.
pub fn register(
    dbos: &DBOS,
    source: Arc<dyn ExportSource>,
) -> dbos::Result<SentEmailExportWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let source = Arc::clone(&source);
        async move {
            let now = dbos::workflow_id()
                .as_deref()
                .and_then(slot_of)
                .unwrap_or_else(Utc::now);
            let pending_source = Arc::clone(&source);
            let days: Vec<NaiveDate> = dbos::step("pending", move || {
                let source = Arc::clone(&pending_source);
                async move {
                    source.pending(now).await.map_err(|error| {
                        tracing::error!(%error, "sent email export: pending days unreadable");
                        dbos::Error::Application(ExportError::Pending)
                    })
                }
            })
            .await?;
            let mut report = RunReport::default();
            for day in days {
                let source = Arc::clone(&source);
                let line: String = dbos::step(&format!("export-{day}"), move || {
                    let source = Arc::clone(&source);
                    async move {
                        source.export(day, now).await.map_err(|error| {
                            tracing::error!(%day, %error, "sent email export: day failed");
                            dbos::Error::Application(ExportError::Export)
                        })
                    }
                })
                .await?;
                report.exported.push((day, line));
            }
            let purge_source = Arc::clone(&source);
            report.purged = dbos::step("purge", move || {
                let source = Arc::clone(&purge_source);
                async move {
                    source.purge(now).await.map_err(|error| {
                        tracing::error!(%error, "sent email export: purge failed");
                        dbos::Error::Application(ExportError::Purge)
                    })
                }
            })
            .await?;
            tracing::info!(
                exported = report.exported.len(),
                purged_days = report.purged.len(),
                "sent email export run finished"
            );
            dbos::Result::<_, ExportError>::Ok(report)
        }
    })
}

/// Start (or join) the run for `slot`.
///
/// # Errors
///
/// DBOS could not start the workflow.
pub async fn fire(
    workflow: &SentEmailExportWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<RunReport, ExportError>, ExportError> {
    let id = workflow_id(slot);
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

/// Start each day's run at [`slot_time`]. A late executor catches up: the
/// run is idempotent, so starting it late only exports what is still pending.
pub async fn schedule(workflow: SentEmailExportWorkflow, stop: CancellationToken) {
    daily::run(
        WORKFLOW,
        slot_time(),
        LateStart::CatchUp,
        Utc::now,
        stop,
        |slot| {
            let workflow = &workflow;
            async move { fire(workflow, slot).await.map(|_| ()) }
        },
    )
    .await;
}

#[cfg(test)]
#[path = "sent_email_export_tests.rs"]
mod tests;
