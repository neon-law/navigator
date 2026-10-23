//! [Mirror Backfill](../../docs/glossary/mirror-backfill.md): copy the
//! SurrealDB rows the PostgreSQL write mirror never saw, in bounded,
//! resumable batches.
//!
//! The write mirror only sees writes made after it was switched on. A
//! backfill walks [`MIRRORS`] parents first. For each group it compares both
//! stores ([`Mirror::parity`]), keeps the divergent keys PostgreSQL holds no
//! row for ([`missing`]), and writes them through the mirror's own
//! [`mirror`] in batches of the run's size. Every batch commits its rows and
//! then advances the group's checkpoint in `mirror_backfill_checkpoint`.
//!
//! A run is resumable by id. Resuming recomputes the group's missing rows,
//! so rows an interrupted batch already wrote are no longer missing and are
//! not written twice; the mirror's upsert is keyed on each row's identity,
//! so even a crash between a batch's rows and its checkpoint cannot
//! duplicate a row. A key that still fails after a reverse-order retry is
//! recorded in `mirror_backfill_failure` by table and UUID only. Rows that
//! differ, and rows only PostgreSQL holds, are reported, never written: the
//! mirror's own `reconcile` owns those repairs.
//!
//! [`plan`] reads both stores and writes nothing, not even a run record.
//! Only one backfill advances at a time: a run holds a PostgreSQL advisory
//! lock for as long as it writes.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::time::Duration;

use sqlx::Row;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

/// Source metadata recreated by PostgreSQL schema migrations, not imported as
/// domain data. Keep this list exact: every other populated source table must
/// have a registered canonical mirror.
const RECREATED_SOURCE_METADATA: &[&str] = &["schema_version"];

/// A hung object backend must fail verification rather than stall a migration gate.
const OBJECT_CHECK_TIMEOUT: Duration = Duration::from_secs(120);

use crate::postgres::PostgresDb;
use crate::postgres_mirror::{mirror, missing, Mirror, MirrorError, MirrorKey, MIRRORS};
use crate::surreal::SurrealDb;

/// Asset object coverage after checking both stores' metadata pointers and
/// reading each distinct retained object from document storage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetObjectVerification {
    pub source_assets: usize,
    pub target_assets: usize,
    pub unique_objects: usize,
    pub verified_bytes: u64,
    pub missing_objects: usize,
    pub unreadable_objects: usize,
    pub size_mismatches: usize,
    pub digest_mismatches: usize,
    pub conflicting_expectations: usize,
    pub invalid_byte_sizes: usize,
    pub purged_assets: usize,
    /// Distinct public logo/font/video keys found across both databases.
    pub public_reference_objects: usize,
    /// Bytes confirmed present by public-asset object metadata; no digest is recorded for these source fields.
    pub public_reference_bytes: u64,
    pub public_missing_objects: usize,
    pub public_unreadable_objects: usize,
    /// Distinct private raw-message keys not already verified as an Asset object.
    pub private_reference_objects: usize,
    /// Bytes confirmed present by private-document object metadata; no digest is recorded for these source fields.
    pub private_reference_bytes: u64,
    pub private_missing_objects: usize,
    pub private_unreadable_objects: usize,
    /// Empty explicit object-key pointers that cannot resolve to a storage object.
    pub invalid_object_keys: usize,
}

impl AssetObjectVerification {
    /// All retained object references resolve to bytes with their recorded
    /// size and SHA-256. Purged assets are intentionally excluded.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.missing_objects == 0
            && self.unreadable_objects == 0
            && self.size_mismatches == 0
            && self.digest_mismatches == 0
            && self.conflicting_expectations == 0
            && self.invalid_byte_sizes == 0
            && self.public_missing_objects == 0
            && self.public_unreadable_objects == 0
            && self.private_missing_objects == 0
            && self.private_unreadable_objects == 0
            && self.invalid_object_keys == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AssetObjectExpectation {
    byte_size: u64,
    sha256_hex: String,
}

#[derive(SurrealValue)]
struct FinanceReference {
    project_code: Option<String>,
    run_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AssetObjectError {
    #[error("could not read SurrealDB asset metadata")]
    Source,
    #[error("could not read PostgreSQL asset metadata")]
    Target(#[from] sqlx::Error),
}

/// Check every retained asset object referenced by either store. The result
/// contains aggregate counts only; object keys, digests, filenames, and row
/// identifiers are never returned or logged.
pub async fn verify_asset_objects(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    document_storage: &dyn cloud::StorageService,
    public_assets_storage: &dyn cloud::StorageService,
) -> Result<AssetObjectVerification, AssetObjectError> {
    #[derive(SurrealValue)]
    struct SourceAsset {
        storage_key: String,
        secondary_storage_key: Option<String>,
        byte_size: i64,
        sha256_hex: String,
        purged_at: Option<surrealdb::types::Datetime>,
    }

    let source_assets: Vec<SourceAsset> = surreal
        .query("SELECT storage_key, secondary_storage_key, byte_size, sha256_hex, purged_at FROM asset")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| AssetObjectError::Source)?
        .take(0)
        .map_err(|_| AssetObjectError::Source)?;
    let target_assets = sqlx::query(
        "SELECT storage_key, secondary_storage_key, byte_size, sha256_hex, purged_at FROM asset",
    )
    .fetch_all(&pg.0)
    .await?;

    let mut report = AssetObjectVerification {
        source_assets: source_assets.len(),
        target_assets: target_assets.len(),
        ..AssetObjectVerification::default()
    };
    let mut expected = BTreeMap::<String, AssetObjectExpectation>::new();
    for asset in source_assets {
        if asset.purged_at.is_some() {
            report.purged_assets += 1;
            continue;
        }
        let byte_size = verified_byte_size(asset.byte_size, &mut report);
        expected_object(
            &mut expected,
            &mut report,
            asset.storage_key,
            byte_size,
            asset.sha256_hex.clone(),
        );
        if let Some(key) = asset.secondary_storage_key {
            expected_object(&mut expected, &mut report, key, byte_size, asset.sha256_hex);
        }
    }
    for asset in target_assets {
        let storage_key: String = asset.try_get("storage_key")?;
        let secondary_storage_key: Option<String> = asset.try_get("secondary_storage_key")?;
        let byte_size: i64 = asset.try_get("byte_size")?;
        let sha256_hex: String = asset.try_get("sha256_hex")?;
        let purged_at: Option<chrono::DateTime<chrono::Utc>> = asset.try_get("purged_at")?;
        if purged_at.is_some() {
            continue;
        }
        let byte_size = verified_byte_size(byte_size, &mut report);
        expected_object(
            &mut expected,
            &mut report,
            storage_key,
            byte_size,
            sha256_hex.clone(),
        );
        if let Some(key) = secondary_storage_key {
            expected_object(&mut expected, &mut report, key, byte_size, sha256_hex);
        }
    }

    report.unique_objects = expected.len();
    let verified_asset_keys = expected
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    check_asset_objects(document_storage, expected, &mut report).await;

    let (public_keys, raw_message_keys, finance_keys, invalid_object_keys) =
        non_asset_object_references(surreal, pg).await?;
    report.invalid_object_keys = invalid_object_keys;
    report.public_reference_objects = public_keys.len();
    check_referenced_objects(
        public_assets_storage,
        public_keys,
        &mut report.public_reference_bytes,
        &mut report.public_missing_objects,
        &mut report.public_unreadable_objects,
    )
    .await;
    let mut private_keys = raw_message_keys;
    private_keys.extend(finance_keys);
    private_keys.retain(|key| !verified_asset_keys.contains(key));
    report.private_reference_objects = private_keys.len();
    check_referenced_objects(
        document_storage,
        private_keys,
        &mut report.private_reference_bytes,
        &mut report.private_missing_objects,
        &mut report.private_unreadable_objects,
    )
    .await;
    Ok(report)
}

#[allow(clippy::too_many_lines)] // Keeps the field-to-storage-lane inventory auditable in one place.
async fn non_asset_object_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<
    (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<String>,
        usize,
    ),
    AssetObjectError,
> {
    #[derive(SurrealValue)]
    struct BrandKeys {
        logo_object_key: Option<String>,
        font_object_key: Option<String>,
    }
    #[derive(SurrealValue)]
    struct OneKey {
        key: String,
    }
    #[derive(SurrealValue)]
    struct OptionalKey {
        key: Option<String>,
    }

    let mut public = std::collections::BTreeSet::new();
    let mut private = std::collections::BTreeSet::new();
    let mut finance = std::collections::BTreeSet::new();
    let mut invalid = 0;
    let brands: Vec<BrandKeys> = surreal
        .query("SELECT logo_object_key, font_object_key FROM brand")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| AssetObjectError::Source)?
        .take(0)
        .map_err(|_| AssetObjectError::Source)?;
    for brand in brands {
        for key in [brand.logo_object_key, brand.font_object_key]
            .into_iter()
            .flatten()
        {
            if key.is_empty() {
                invalid += 1;
            } else {
                public.insert(key);
            }
        }
    }
    let videos: Vec<OneKey> = surreal
        .query("SELECT object_key AS key FROM blog_video")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| AssetObjectError::Source)?
        .take(0)
        .map_err(|_| AssetObjectError::Source)?;
    for row in videos {
        if row.key.is_empty() {
            invalid += 1;
        } else {
            public.insert(row.key);
        }
    }
    let messages: Vec<OptionalKey> = surreal
        .query("SELECT raw_storage_key AS key FROM email_conversation_message")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| AssetObjectError::Source)?
        .take(0)
        .map_err(|_| AssetObjectError::Source)?;
    for key in messages.into_iter().filter_map(|row| row.key) {
        if key.is_empty() {
            invalid += 1;
        } else {
            private.insert(key);
        }
    }

    let finance_rows: Vec<FinanceReference> = surreal
        .query("SELECT project_id.code AS project_code, run_id FROM finance_aggregate")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| AssetObjectError::Source)?
        .take(0)
        .map_err(|_| AssetObjectError::Source)?;
    for row in finance_rows {
        if let Some(project_code) = row.project_code.filter(|code| !code.is_empty()) {
            finance.insert(crate::finance_aggregate::input_key(&project_code));
            finance.insert(crate::finance_aggregate::report_key(
                &project_code,
                &row.run_id,
            ));
        } else {
            invalid += 1;
        }
    }

    for row in sqlx::query("SELECT logo_object_key, font_object_key FROM brand")
        .fetch_all(&pg.0)
        .await?
    {
        for field in ["logo_object_key", "font_object_key"] {
            if let Some(key) = row.try_get::<Option<String>, _>(field)? {
                if key.is_empty() {
                    invalid += 1;
                } else {
                    public.insert(key);
                }
            }
        }
    }
    for row in sqlx::query("SELECT object_key FROM blog_video")
        .fetch_all(&pg.0)
        .await?
    {
        let key: String = row.try_get("object_key")?;
        if key.is_empty() {
            invalid += 1;
        } else {
            public.insert(key);
        }
    }
    for row in sqlx::query("SELECT raw_storage_key FROM email_conversation_message")
        .fetch_all(&pg.0)
        .await?
    {
        if let Some(key) = row.try_get::<Option<String>, _>("raw_storage_key")? {
            if key.is_empty() {
                invalid += 1;
            } else {
                private.insert(key);
            }
        }
    }
    for row in sqlx::query(
        "SELECT p.code AS project_code, a.run_id FROM finance_aggregate AS a \
         LEFT JOIN project AS p ON p.id = a.project_id",
    )
    .fetch_all(&pg.0)
    .await?
    {
        let project_code: Option<String> = row.try_get("project_code")?;
        let run_id: String = row.try_get("run_id")?;
        if let Some(project_code) = project_code.filter(|code| !code.is_empty()) {
            finance.insert(crate::finance_aggregate::input_key(&project_code));
            finance.insert(crate::finance_aggregate::report_key(&project_code, &run_id));
        } else {
            invalid += 1;
        }
    }
    Ok((public, private, finance, invalid))
}

async fn check_referenced_objects(
    storage: &dyn cloud::StorageService,
    keys: std::collections::BTreeSet<String>,
    verified_bytes: &mut u64,
    missing_objects: &mut usize,
    unreadable_objects: &mut usize,
) {
    for key in keys {
        match object_check_with_timeout(storage.head(&key), OBJECT_CHECK_TIMEOUT).await {
            Ok(Some(head)) => *verified_bytes = verified_bytes.saturating_add(head.size_bytes),
            Ok(None) | Err(ObjectCheckError::Backend(cloud::StorageError::NotFound(_))) => {
                *missing_objects += 1;
            }
            Err(_) => *unreadable_objects += 1,
        }
    }
}

#[derive(Debug)]
enum ObjectCheckError {
    Backend(cloud::StorageError),
    TimedOut,
}

async fn object_check_with_timeout<T>(
    check: impl Future<Output = Result<T, cloud::StorageError>>,
    timeout: Duration,
) -> Result<T, ObjectCheckError> {
    tokio::time::timeout(timeout, check)
        .await
        .map_err(|_| ObjectCheckError::TimedOut)?
        .map_err(ObjectCheckError::Backend)
}

fn verified_byte_size(byte_size: i64, report: &mut AssetObjectVerification) -> u64 {
    if let Ok(byte_size) = u64::try_from(byte_size) {
        byte_size
    } else {
        report.invalid_byte_sizes += 1;
        u64::MAX
    }
}

async fn check_asset_objects(
    storage: &dyn cloud::StorageService,
    expected: BTreeMap<String, AssetObjectExpectation>,
    report: &mut AssetObjectVerification,
) {
    for (key, expectation) in expected {
        match object_check_with_timeout(storage.get(&key), OBJECT_CHECK_TIMEOUT).await {
            Ok(object) => {
                let actual_size = u64::try_from(object.bytes.len()).unwrap_or(u64::MAX);
                let actual_digest = crate::assets::sha256_hex(&object.bytes);
                let size_matches = actual_size == expectation.byte_size;
                let digest_matches = actual_digest == expectation.sha256_hex;
                if !size_matches {
                    report.size_mismatches += 1;
                }
                if !digest_matches {
                    report.digest_mismatches += 1;
                }
                if size_matches && digest_matches {
                    report.verified_bytes = report.verified_bytes.saturating_add(actual_size);
                }
            }
            Err(ObjectCheckError::Backend(cloud::StorageError::NotFound(_))) => {
                report.missing_objects += 1;
            }
            Err(_) => report.unreadable_objects += 1,
        }
    }
}

fn expected_object(
    expected: &mut BTreeMap<String, AssetObjectExpectation>,
    report: &mut AssetObjectVerification,
    key: String,
    byte_size: u64,
    sha256_hex: String,
) {
    let value = AssetObjectExpectation {
        byte_size,
        sha256_hex,
    };
    if let Some(existing) = expected.get(&key) {
        if existing != &value {
            report.conflicting_expectations += 1;
        }
    } else {
        expected.insert(key, value);
    }
}

/// The default number of rows a batch writes before its checkpoint.
pub const DEFAULT_BATCH_SIZE: usize = 500;

/// The session advisory lock every advancing backfill holds.
const LOCK: i64 = 0x6d69_7272_6f72_6266; // "mirrorbf"
/// Maximum UUID keys included in one post-run parity table summary.
const MAX_PARITY_IDS: usize = 50;

/// Why a backfill could not run. Carries no row values.
#[derive(Debug, thiserror::Error)]
pub enum BackfillError {
    #[error("{0}")]
    Mirror(#[from] MirrorError),
    #[error("another backfill is advancing; retry when it finishes")]
    Busy,
    #[error("no backfill run {0}")]
    UnknownRun(Uuid),
    #[error("a batch size must be at least 1")]
    BatchSize,
    #[error("could not inventory SurrealDB source tables")]
    SourceInventory,
    #[error("could not query SurrealDB source table {0} count")]
    SourceTableQuery(String),
    #[error("could not read SurrealDB source table {0} count")]
    SourceTableRead(String),
    #[error("could not decode SurrealDB source table {0} count")]
    SourceTableCount(String),
    #[error(
        "populated source tables lack PostgreSQL mirror coverage (archived raw rows are not canonical import coverage): {0:?}"
    )]
    UncoveredTables(Vec<UncoveredTable>),
}

impl From<sqlx::Error> for BackfillError {
    fn from(error: sqlx::Error) -> Self {
        Self::Mirror(error.into())
    }
}

/// One table's share of a backfill, named by UUID only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePlan {
    pub mirror: &'static str,
    pub table: &'static str,
    pub surreal: usize,
    pub postgres: usize,
    /// Rows SurrealDB holds and PostgreSQL lacks: what a run writes.
    pub missing: usize,
    /// Rows both hold with a column that differs: left to `reconcile`.
    pub differing: usize,
    /// Rows only PostgreSQL holds: left to `reconcile`.
    pub extra: usize,
    /// Batches the missing rows need at the plan's batch size.
    pub batches: usize,
    /// Missing rows named by UUID, sorted.
    pub missing_ids: Vec<String>,
    /// Missing rows whose key is not a UUID, counted only.
    pub unnamed: usize,
}

/// A populated source table that no registered PostgreSQL mirror imports.
/// Carries only the table name and row count, never row contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncoveredTable {
    pub table: String,
    pub rows: usize,
}

/// A deterministic, read-only account of what a backfill would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub batch_size: usize,
    pub tables: Vec<TablePlan>,
    /// Source orphans and target parent rows still absent, grouped by table and field.
    pub unresolved_references: Vec<crate::postgres_mirror::records::UnresolvedReference>,
}

impl Plan {
    /// Every row a run would write.
    #[must_use]
    pub fn missing(&self) -> usize {
        self.tables.iter().map(|table| table.missing).sum()
    }

    /// Whether both stores agree on every mirrored row and declared reference.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.tables.is_empty()
            && self
                .tables
                .iter()
                .all(|table| table.missing == 0 && table.differing == 0 && table.extra == 0)
            && self.unresolved_references.is_empty()
    }
}

/// Compare every mirror and list what a backfill would write. Writes
/// nothing to either store.
///
/// # Errors
///
/// A batch size of zero, or a store that could not be read.
pub async fn plan(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    batch_size: usize,
) -> Result<Plan, BackfillError> {
    if batch_size == 0 {
        return Err(BackfillError::BatchSize);
    }
    let mut tables = Vec::new();
    let mut covered = BTreeSet::new();
    for group in MIRRORS {
        let report = group.parity(surreal, pg).await?;
        for table in &report.tables {
            covered.insert(table.table);
            let drift = crate::postgres_mirror::drift(pg, table).await?;
            let keys = missing(pg, table).await?;
            let mut missing_ids: Vec<String> =
                keys.iter().filter_map(MirrorKey::report_id).collect();
            missing_ids.sort();
            tables.push(TablePlan {
                mirror: group.name,
                table: table.table,
                surreal: table.source_rows,
                postgres: table.target_rows,
                missing: keys.len(),
                differing: drift.differing,
                extra: drift.extra,
                batches: keys.len().div_ceil(batch_size),
                unnamed: keys.len() - missing_ids.len(),
                missing_ids,
            });
        }
    }
    ensure_source_coverage(surreal, &covered).await?;
    let unresolved_references =
        crate::postgres_mirror::records::unresolved_references(surreal, pg).await?;
    Ok(Plan {
        batch_size,
        tables,
        unresolved_references,
    })
}

/// Names the source tables each registered mirror handles.
async fn mirror_coverage(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<BTreeSet<&'static str>, BackfillError> {
    let mut covered = BTreeSet::new();
    for group in MIRRORS {
        let report = group.parity(surreal, pg).await?;
        covered.extend(report.tables.iter().map(|table| table.table));
    }
    Ok(covered)
}

/// Fail closed if a live source table contains rows outside mirror coverage.
/// Derived views are excluded because their rows are rebuilt from canonical
/// source tables; analytical archives are not considered import coverage.
async fn ensure_source_coverage(
    surreal: &SurrealDb,
    covered: &BTreeSet<&'static str>,
) -> Result<(), BackfillError> {
    let tables: Option<BTreeMap<String, String>> = surreal
        .query("RETURN (INFO FOR DB).tables")
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| BackfillError::SourceInventory)?
        .take(0)
        .map_err(|_| BackfillError::SourceInventory)?;
    let derived: BTreeSet<String> = crate::schema::derived_tables()
        .into_iter()
        .map(|table| table.name)
        .collect();
    let mut uncovered = Vec::new();
    for table in tables.unwrap_or_default().keys() {
        if covered.contains(table.as_str())
            || derived.contains(table)
            || RECREATED_SOURCE_METADATA.contains(&table.as_str())
        {
            continue;
        }
        let mut response = surreal
            .query("SELECT VALUE count() FROM type::table($table) GROUP ALL")
            .bind(("table", table.clone()))
            .await
            .and_then(surrealdb::IndexedResults::check)
            .map_err(|_| BackfillError::SourceTableQuery(table.clone()))?;
        let counts: Vec<surrealdb::types::Value> = response
            .take(0)
            .map_err(|_| BackfillError::SourceTableRead(table.clone()))?;
        let rows = match counts.into_iter().next() {
            None => 0,
            Some(value) => {
                let value = value.into_json_value();
                let count = value
                    .as_u64()
                    .or_else(|| value.get("count").and_then(serde_json::Value::as_u64))
                    .and_then(|count| usize::try_from(count).ok())
                    .ok_or_else(|| BackfillError::SourceTableCount(table.clone()))?;
                count
            }
        };
        if rows > 0 {
            uncovered.push(UncoveredTable {
                table: table.clone(),
                rows,
            });
        }
    }
    uncovered.sort_by(|left, right| left.table.cmp(&right.table));
    if uncovered.is_empty() {
        Ok(())
    } else {
        Err(BackfillError::UncoveredTables(uncovered))
    }
}

/// Where a run stands after one invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Every group checkpointed; the run is closed.
    Completed,
    /// The invocation stopped at its batch limit; resume by id.
    Paused,
}

impl RunStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Paused => "paused",
        }
    }
}

/// One group's checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupProgress {
    pub mirror: String,
    pub batches: usize,
    pub applied: usize,
    pub failed: usize,
    pub completed: bool,
}

/// One row still missing when its group completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub mirror: String,
    pub table: String,
    /// The row's UUID, or `None` for a key that is not a UUID.
    pub id: Option<String>,
    /// The sanitized mirror error.
    pub reason: String,
}

/// One table's parity after a backfill invocation. Divergent keys are safe
/// report identifiers only; keys that could disclose client information are
/// counted without being rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParityTableReport {
    pub mirror: String,
    pub table: String,
    pub surreal: usize,
    pub postgres: usize,
    pub missing: usize,
    pub differing: usize,
    pub extra: usize,
    pub divergent_ids: Vec<String>,
    pub omitted_divergent_ids: usize,
    pub withheld_non_uuid_keys: usize,
}

/// A run's state, read back from its checkpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub run_id: Uuid,
    pub status: RunStatus,
    pub batch_size: usize,
    /// Batches this invocation wrote.
    pub batches_this_invocation: usize,
    pub groups: Vec<GroupProgress>,
    pub failures: Vec<Failure>,
    /// Fresh source/target parity after this invocation. A checkpoint-only
    /// report has no parity because it cannot connect to the source store.
    pub parity: Vec<ParityTableReport>,
    /// Dangling source links and links whose target has not reached PostgreSQL.
    pub unresolved_references: Vec<crate::postgres_mirror::records::UnresolvedReference>,
}

impl RunReport {
    /// Whether the run closed with every mirrored table in parity.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.status == RunStatus::Completed
            && self.failures.is_empty()
            && !self.parity.is_empty()
            && self
                .parity
                .iter()
                .all(|table| table.missing == 0 && table.differing == 0 && table.extra == 0)
            && self.unresolved_references.is_empty()
    }
}

/// Start a run and advance it, stopping after `max_batches` batches when
/// given.
///
/// # Errors
///
/// A batch size of zero, another backfill holding the lock, or a store
/// that could not be read or written.
pub async fn start(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    batch_size: usize,
    max_batches: Option<usize>,
) -> Result<RunReport, BackfillError> {
    let size = i32::try_from(batch_size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or(BackfillError::BatchSize)?;
    let run_id = Uuid::new_v4();
    locked(surreal, pg, run_id, async {
        sqlx::query(
            "INSERT INTO mirror_backfill_run (id, batch_size, status) VALUES ($1, $2, 'running')",
        )
        .bind(run_id)
        .bind(size)
        .execute(&pg.0)
        .await?;
        Box::pin(advance(surreal, pg, run_id, max_batches)).await
    })
    .await
}

/// Advance an existing run from its checkpoints. A completed run is read
/// back unchanged.
///
/// # Errors
///
/// An unknown run, another backfill holding the lock, or a store that could
/// not be read or written.
pub async fn resume(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    run_id: Uuid,
    max_batches: Option<usize>,
) -> Result<RunReport, BackfillError> {
    locked(
        surreal,
        pg,
        run_id,
        Box::pin(advance(surreal, pg, run_id, max_batches)),
    )
    .await
}

/// Run `work` under the backfill lock, then read the run back.
async fn locked(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    run_id: Uuid,
    work: impl std::future::Future<Output = Result<usize, BackfillError>>,
) -> Result<RunReport, BackfillError> {
    let mut lock = pg.0.acquire().await?;
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LOCK)
        .fetch_one(&mut *lock)
        .await?;
    if !held {
        return Err(BackfillError::Busy);
    }
    let advanced = async {
        let covered = mirror_coverage(surreal, pg).await?;
        ensure_source_coverage(surreal, &covered).await?;
        let batches = work.await?;
        let parity = parity_report(surreal, pg).await?;
        let unresolved_references =
            crate::postgres_mirror::records::unresolved_references(surreal, pg).await?;
        Ok::<_, BackfillError>((batches, parity, unresolved_references))
    }
    .await;
    let released = sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(LOCK)
        .execute(&mut *lock)
        .await;
    let (batches, parity, unresolved_references) = advanced?;
    released?;
    let mut report = report(pg, run_id).await?;
    report.batches_this_invocation = batches;
    report.parity = parity;
    report.unresolved_references = unresolved_references;
    Ok(report)
}

/// Compare every mirrored table after a run, preserving only report-safe row
/// keys and counts.
async fn parity_report(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<ParityTableReport>, BackfillError> {
    let mut tables = Vec::new();
    for group in MIRRORS {
        let report = group.parity(surreal, pg).await?;
        for table in report.tables {
            let drift = crate::postgres_mirror::drift(pg, &table).await?;
            let (divergent_ids, omitted_divergent_ids, withheld_non_uuid_keys) =
                safe_key_sample(&table.divergent);
            tables.push(ParityTableReport {
                mirror: group.name.to_string(),
                table: table.table.to_string(),
                surreal: table.source_rows,
                postgres: table.target_rows,
                missing: drift.missing,
                differing: drift.differing,
                extra: drift.extra,
                divergent_ids,
                omitted_divergent_ids,
                withheld_non_uuid_keys,
            });
        }
    }
    Ok(tables)
}

/// Return a bounded sample of UUID keys, omitting all other key forms.
fn safe_key_sample(keys: &[MirrorKey]) -> (Vec<String>, usize, usize) {
    let mut safe_ids: Vec<String> = keys
        .iter()
        .filter_map(MirrorKey::report_id)
        .filter(|id| Uuid::parse_str(id).is_ok())
        .collect();
    safe_ids.sort();
    let total_safe_ids = safe_ids.len();
    let sample: Vec<String> = safe_ids.into_iter().take(MAX_PARITY_IDS).collect();
    let omitted = total_safe_ids.saturating_sub(sample.len());
    let withheld_non_uuid = keys.len().saturating_sub(total_safe_ids);
    (sample, omitted, withheld_non_uuid)
}

/// Write every group's missing rows, checkpointing each batch. Returns the
/// batches written.
async fn advance(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    run_id: Uuid,
    max_batches: Option<usize>,
) -> Result<usize, BackfillError> {
    let run = sqlx::query("SELECT batch_size, status FROM mirror_backfill_run WHERE id = $1")
        .bind(run_id)
        .fetch_optional(&pg.0)
        .await?
        .ok_or(BackfillError::UnknownRun(run_id))?;
    let status: String = run.try_get("status")?;
    if status == "completed" {
        return Ok(0);
    }
    let batch_size = usize::try_from(run.try_get::<i32, _>("batch_size")?)
        .unwrap_or(1)
        .max(1);
    let mut written = 0;
    for group in MIRRORS {
        sqlx::query(
            "INSERT INTO mirror_backfill_checkpoint (run_id, mirror) VALUES ($1, $2) \
             ON CONFLICT (run_id, mirror) DO NOTHING",
        )
        .bind(run_id)
        .bind(group.name)
        .execute(&pg.0)
        .await?;
        let done: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
            "SELECT completed_at FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = $2",
        )
        .bind(run_id)
        .bind(group.name)
        .fetch_one(&pg.0)
        .await?;
        if done.is_some() {
            continue;
        }
        let keys = missing_keys(surreal, pg, group).await?;
        let mut failed = Vec::new();
        for batch in keys.chunks(batch_size) {
            if max_batches.is_some_and(|limit| written >= limit) {
                touch(pg, run_id).await?;
                return Ok(written);
            }
            let mut applied = 0_i32;
            for key in batch {
                match Box::pin(mirror(surreal, pg, key)).await {
                    Ok(_) => applied += 1,
                    Err(_) => failed.push(key.clone()),
                }
            }
            sqlx::query(
                "UPDATE mirror_backfill_checkpoint SET batches = batches + 1, \
                 applied = applied + $3, updated_at = now() WHERE run_id = $1 AND mirror = $2",
            )
            .bind(run_id)
            .bind(group.name)
            .bind(applied)
            .execute(&pg.0)
            .await?;
            written += 1;
        }
        complete_group(surreal, pg, run_id, group, failed).await?;
    }
    sqlx::query(
        "UPDATE mirror_backfill_run SET status = 'completed', completed_at = now(), \
         updated_at = now() WHERE id = $1",
    )
    .bind(run_id)
    .execute(&pg.0)
    .await?;
    Ok(written)
}

/// The group's rows SurrealDB holds and PostgreSQL lacks, parents first.
async fn missing_keys(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    group: &Mirror,
) -> Result<Vec<MirrorKey>, BackfillError> {
    let report = group.parity(surreal, pg).await?;
    let mut keys = Vec::new();
    for table in &report.tables {
        keys.extend(missing(pg, table).await?);
    }
    Ok(keys)
}

/// Retry a group's failed keys children first, so a parent refused for a
/// missing child converges, then record what is still missing.
async fn complete_group(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    run_id: Uuid,
    group: &Mirror,
    failed: Vec<MirrorKey>,
) -> Result<(), BackfillError> {
    let mut reasons = Vec::new();
    let mut recovered = 0_i32;
    for key in failed.iter().rev() {
        match Box::pin(mirror(surreal, pg, key)).await {
            Ok(_) => recovered += 1,
            Err(error) => reasons.push((key.clone(), error.to_string())),
        }
    }
    // A key an earlier invocation failed on may have converged since; only
    // what is missing now is a failure.
    let still: BTreeSet<MirrorKey> = missing_keys(surreal, pg, group)
        .await?
        .into_iter()
        .collect();
    reasons.retain(|(key, _)| still.contains(key));
    let mut tx = pg.0.begin().await?;
    sqlx::query("DELETE FROM mirror_backfill_failure WHERE run_id = $1 AND mirror = $2")
        .bind(run_id)
        .bind(group.name)
        .execute(&mut *tx)
        .await?;
    for (key, reason) in &reasons {
        sqlx::query(
            "INSERT INTO mirror_backfill_failure (id, run_id, mirror, row_table, row_id, reason) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(group.name)
        .bind(key.table())
        .bind(key.report_id())
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE mirror_backfill_checkpoint SET applied = applied + $3, failed = $4, \
         completed_at = now(), updated_at = now() WHERE run_id = $1 AND mirror = $2",
    )
    .bind(run_id)
    .bind(group.name)
    .bind(recovered)
    .bind(i32::try_from(reasons.len()).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn touch(pg: &PostgresDb, run_id: Uuid) -> Result<(), BackfillError> {
    sqlx::query("UPDATE mirror_backfill_run SET updated_at = now() WHERE id = $1")
        .bind(run_id)
        .execute(&pg.0)
        .await?;
    Ok(())
}

/// Read a run's checkpoints and failures back.
///
/// # Errors
///
/// An unknown run, or PostgreSQL could not be read.
pub async fn report(pg: &PostgresDb, run_id: Uuid) -> Result<RunReport, BackfillError> {
    let run = sqlx::query("SELECT batch_size, status FROM mirror_backfill_run WHERE id = $1")
        .bind(run_id)
        .fetch_optional(&pg.0)
        .await?
        .ok_or(BackfillError::UnknownRun(run_id))?;
    let status = if run.try_get::<String, _>("status")? == "completed" {
        RunStatus::Completed
    } else {
        RunStatus::Paused
    };
    let count = |value: i32| usize::try_from(value).unwrap_or_default();
    let order: Vec<&str> = MIRRORS.iter().map(|group| group.name).collect();
    let mut groups = sqlx::query(
        "SELECT mirror, batches, applied, failed, completed_at IS NOT NULL AS completed \
         FROM mirror_backfill_checkpoint WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(GroupProgress {
            mirror: row.try_get("mirror")?,
            batches: count(row.try_get("batches")?),
            applied: count(row.try_get("applied")?),
            failed: count(row.try_get("failed")?),
            completed: row.try_get("completed")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()?;
    groups.sort_by_key(|group| order.iter().position(|name| *name == group.mirror));
    let mut failures = sqlx::query(
        "SELECT mirror, row_table, row_id, reason FROM mirror_backfill_failure WHERE run_id = $1",
    )
    .bind(run_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(Failure {
            mirror: row.try_get("mirror")?,
            table: row.try_get("row_table")?,
            id: row.try_get("row_id")?,
            reason: row.try_get("reason")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()?;
    failures.sort_by(|left, right| {
        (&left.mirror, &left.table, &left.id).cmp(&(&right.mirror, &right.table, &right.id))
    });
    Ok(RunReport {
        run_id,
        status,
        batch_size: count(run.try_get("batch_size")?),
        batches_this_invocation: 0,
        groups,
        failures,
        parity: Vec::new(),
        unresolved_references: Vec::new(),
    })
}

#[cfg(test)]
#[path = "postgres_backfill_tests.rs"]
mod tests;
