//! Project Recovery Archives: verified, immutable generations of one Project's
//! recoverable record in the private archived-projects bucket.
//!
//! A generation holds every Project document revision (soft-deleted ones
//! included; a purged revision keeps only its tombstone row), the Notation
//! source and output, the Project-scoped rows a restore needs, and a verified
//! reference to the Project repository's Git bundle backup. It never holds
//! credentials, other Projects' or Firms' records, or DBOS internal state.
//!
//! The operation is resumable. PostgreSQL holds the plan and one checkpoint
//! per item ([`project_archive_item`](../../postgres/migrations)); bytes are
//! copied to content-addressed keys, read back, and checked by size and
//! SHA-256 before an item is marked verified. A retry after a crash, an
//! unknown provider outcome, or a lost checkpoint repeats only the
//! idempotent copy-and-verify. A Project that changes while it is copied is
//! re-planned with a new cutoff. A generation commits only when every expected
//! item verifies and its content-addressed manifest has been written and read
//! back; once committed its rows are immutable.
//!
//! [`restore`] rebuilds the Project from one committed generation into any
//! target store, preserving every identity, revision, checksum, provenance
//! link, visibility, Soft Deletion stamp, and Legal Hold. It never overwrites
//! a different existing row or object.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::{StorageError, StorageService};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::assets::{Asset, RowRestore};
use crate::firm_capability::FirmCapability;
use crate::notation_documents::{NotationDocument, NotationDocumentVersion};
use crate::notations::Notation;
use crate::persons::Role;
use crate::postgres::PostgresDb;
use crate::projects::Project;
use crate::surreal::SurrealDb;

/// The manifest and restore-metadata format this build writes and reads.
pub const FORMAT: u32 = 1;
/// The Git bundle receipt format `navigator ops forgejo bundle archive` writes.
pub const SOURCE_BACKUP_FORMAT: u32 = 1;
/// How many times one run re-plans a Project that keeps changing.
const MAX_PLANS: usize = 3;
const OPERATION_KEY_MAX: usize = 200;
const OBJECT_CONTENT_TYPE: &str = "application/octet-stream";
const JSON_CONTENT_TYPE: &str = "application/json";

/// The closed vocabulary of [`ArchiveItem::kind`].
pub mod kind {
    /// One Project document revision's bytes.
    pub const DOCUMENT_REVISION: &str = "document_revision";
    /// A Notation's Markdown source, anchor manifest, or imported original.
    pub const NOTATION_SOURCE: &str = "notation_source";
    /// A Notation's rendered, signed, or completion-certificate PDF.
    pub const NOTATION_OUTPUT: &str = "notation_output";
    /// The Project-scoped rows a restore writes back.
    pub const RESTORE_METADATA: &str = "restore_metadata";
    /// The verified Git bundle backup of the Project repository.
    pub const SOURCE_BACKUP: &str = "source_backup";
}

/// Paths inside a generation's manifest.
const RESTORE_METADATA_PATH: &str = "restore/metadata.json";
const SOURCE_BACKUP_PATH: &str = "source/repository.bundle";

/// Proof that one person may manage one Firm's recovery archives. Only
/// [`authorize`] builds it, so every archive command is Firm-scoped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveAuthority {
    firm_id: Uuid,
    person_id: Uuid,
}

impl ArchiveAuthority {
    #[must_use]
    pub const fn firm_id(&self) -> Uuid {
        self.firm_id
    }

    #[must_use]
    pub const fn person_id(&self) -> Uuid {
        self.person_id
    }
}

/// Resolve [`FirmCapability::ManageRecoveryArchives`] for `person_id` on
/// `firm_id`.
///
/// # Errors
/// [`ArchiveError::Forbidden`] when the capability is refused or the Firm does
/// not exist, or a database error.
pub async fn authorize(
    surreal: &SurrealDb,
    role: Role,
    person_id: Uuid,
    firm_id: Uuid,
) -> Result<ArchiveAuthority, ArchiveError> {
    let decision = FirmCapability::ManageRecoveryArchives
        .resolve(surreal, role, Some(person_id), firm_id)
        .await
        .map_err(|error| ArchiveError::Store(error.to_string()))?;
    if decision.is_allowed() {
        Ok(ArchiveAuthority { firm_id, person_id })
    } else {
        Err(ArchiveError::Forbidden)
    }
}

/// The private receipt `navigator ops forgejo bundle archive` writes after it
/// restored and verified a Git bundle. A generation records it as the
/// Project's source and history backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBackupReference {
    pub format: u32,
    pub object: String,
    pub generation: u64,
    pub size_bytes: u64,
    pub sha256: String,
    pub head: String,
    pub refs: String,
}

impl SourceBackupReference {
    fn validate(&self) -> Result<(), ArchiveError> {
        if self.format != SOURCE_BACKUP_FORMAT {
            return Err(ArchiveError::InvalidSourceBackup(
                "unsupported receipt format",
            ));
        }
        if self.object.trim().is_empty() || self.generation == 0 || self.size_bytes == 0 {
            return Err(ArchiveError::InvalidSourceBackup("incomplete receipt"));
        }
        if !is_sha256(&self.sha256) {
            return Err(ArchiveError::InvalidSourceBackup("malformed digest"));
        }
        if !self.head.starts_with("refs/") || self.refs.trim().is_empty() {
            return Err(ArchiveError::InvalidSourceBackup("missing refs"));
        }
        Ok(())
    }
}

/// The stores one archive command reads and writes. `archive` is the
/// archived-projects lane; `source_backups` is the Git bundle lane and is
/// needed only for a Project with a repository.
pub struct ArchiveStores<'a> {
    pub surreal: &'a SurrealDb,
    pub pg: &'a PostgresDb,
    pub documents: &'a Arc<dyn StorageService>,
    pub archive: &'a Arc<dyn StorageService>,
    pub source_backups: Option<&'a Arc<dyn StorageService>>,
}

/// Where a generation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    /// Requested; nothing planned yet.
    Pending,
    /// Planned and being copied; not a usable archive.
    Copying,
    /// Every item and the manifest verified. Immutable.
    Committed,
}

impl GenerationState {
    fn parse(value: &str) -> Result<Self, ArchiveError> {
        match value {
            "pending" => Ok(Self::Pending),
            "copying" => Ok(Self::Copying),
            "committed" => Ok(Self::Committed),
            _ => Err(ArchiveError::Database("unknown generation state".into())),
        }
    }
}

/// One generation row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Generation {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub project_id: Uuid,
    pub operation_key: String,
    pub state: GenerationState,
    pub requested_by_person_id: Uuid,
    pub requested_at: DateTime<Utc>,
    pub cutoff_at: Option<DateTime<Utc>>,
    pub inventory_sha256: Option<String>,
    pub manifest_key: Option<String>,
    pub manifest_sha256: Option<String>,
    pub manifest_size_bytes: Option<i64>,
    pub item_count: Option<i32>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub committed_at: Option<DateTime<Utc>>,
    #[serde(skip)]
    source_backup: Option<SourceBackupReference>,
}

impl Generation {
    /// The source backup this generation records, if the Project has a
    /// repository.
    #[must_use]
    pub fn source_backup(&self) -> Option<&SourceBackupReference> {
        self.source_backup.as_ref()
    }
}

/// One expected item and its checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveItem {
    pub path: String,
    pub kind: String,
    pub asset_id: Option<Uuid>,
    /// The documents-storage key the bytes were read from, when they came
    /// from there.
    pub source_key: Option<String>,
    /// The archived object key; for a source backup, the Git bundle object.
    pub object_key: String,
    pub sha256: String,
    pub size_bytes: i64,
    #[serde(skip)]
    pub verified_at: Option<DateTime<Utc>>,
}

/// The committed manifest, written as JSON at a content-addressed key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub generation_id: Uuid,
    pub operation_key: String,
    pub firm_id: Uuid,
    pub project_id: Uuid,
    pub project_code: String,
    pub cutoff_at: DateTime<Utc>,
    pub inventory_sha256: String,
    pub source_backup: Option<SourceBackupReference>,
    pub items: Vec<ArchiveItem>,
}

/// The Project-scoped rows a restore writes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreMetadata {
    pub format: u32,
    pub project: Project,
    /// Project document revisions and the Notation source assets they need,
    /// ordered by id.
    pub assets: Vec<Asset>,
    pub notations: Vec<Notation>,
    pub notation_documents: Vec<NotationDocument>,
    pub notation_document_versions: Vec<NotationDocumentVersion>,
}

/// Why an archive command stopped. Messages name paths and identifiers, never
/// document content.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("not authorized to manage this Firm's recovery archives")]
    Forbidden,
    #[error("project not found")]
    ProjectNotFound,
    #[error("the project belongs to no Firm")]
    NoFirm,
    #[error("operation keys are 1 to 200 visible ASCII characters")]
    InvalidOperationKey,
    #[error("the operation key already names a different archive request")]
    OperationKeyConflict,
    #[error("a project with a repository needs a verified source backup receipt")]
    SourceBackupRequired,
    #[error("invalid source backup receipt: {0}")]
    InvalidSourceBackup(&'static str),
    #[error("the source backup storage lane is not configured")]
    SourceBackupStorageMissing,
    #[error("source object for {0} is missing")]
    SourceMissing(String),
    #[error("source object for {0} does not match its recorded size and digest")]
    SourceCorrupt(String),
    #[error("archived object for {0} is missing")]
    ArchiveMissing(String),
    #[error("archived object for {0} does not match its recorded size and digest")]
    ArchiveCorrupt(String),
    #[error("the project kept changing while it was archived; run the operation again")]
    Unstable,
    #[error("archive generation not found")]
    GenerationNotFound,
    #[error("the archive generation is not committed")]
    NotCommitted,
    #[error("the manifest does not match the committed generation")]
    ManifestMismatch,
    #[error("restore target lacks {0}")]
    RestorePrerequisite(&'static str),
    #[error("restore target already holds a different {0}")]
    RestoreConflict(String),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("database: {0}")]
    Database(String),
    #[error("store: {0}")]
    Store(String),
}

impl From<sqlx::Error> for ArchiveError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

fn store_error(error: impl std::fmt::Display) -> ArchiveError {
    ArchiveError::Store(error.to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn size_of(bytes: &[u8]) -> i64 {
    i64::try_from(bytes.len()).unwrap_or(i64::MAX)
}

/// `firms/<firm>/projects/<project>/`, the prefix every object of one
/// Project's generations lives under.
#[must_use]
pub fn project_prefix(firm_id: Uuid, project_id: Uuid) -> String {
    format!("firms/{firm_id}/projects/{project_id}/")
}

/// The content-addressed key of one archived object.
#[must_use]
pub fn object_key(firm_id: Uuid, project_id: Uuid, sha256: &str) -> String {
    format!("{}objects/{sha256}", project_prefix(firm_id, project_id))
}

fn manifest_key(generation: &Generation, sha256: &str) -> String {
    format!(
        "{}generations/{}/manifest-{sha256}.json",
        project_prefix(generation.firm_id, generation.project_id),
        generation.id
    )
}

const GENERATION_COLUMNS: &str = "id, firm_id, project_id, operation_key, state, \
     requested_by_person_id, requested_at, source_backup, cutoff_at, inventory_sha256, \
     manifest_key, manifest_sha256, manifest_size_bytes, item_count, attempts, last_error, \
     committed_at";

fn generation_from(row: &sqlx::postgres::PgRow) -> Result<Generation, ArchiveError> {
    let source_backup: Option<serde_json::Value> = row.try_get("source_backup")?;
    Ok(Generation {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        project_id: row.try_get("project_id")?,
        operation_key: row.try_get("operation_key")?,
        state: GenerationState::parse(row.try_get::<&str, _>("state")?)?,
        requested_by_person_id: row.try_get("requested_by_person_id")?,
        requested_at: row.try_get("requested_at")?,
        cutoff_at: row.try_get("cutoff_at")?,
        inventory_sha256: row.try_get("inventory_sha256")?,
        manifest_key: row.try_get("manifest_key")?,
        manifest_sha256: row.try_get("manifest_sha256")?,
        manifest_size_bytes: row.try_get("manifest_size_bytes")?,
        item_count: row.try_get("item_count")?,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
        committed_at: row.try_get("committed_at")?,
        source_backup: source_backup
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ArchiveError::Database("unreadable source backup reference".into()))?,
    })
}

async fn load(pg: &PostgresDb, id: Uuid) -> Result<Generation, ArchiveError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GENERATION_COLUMNS} FROM project_archive_generation WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(ArchiveError::GenerationNotFound)?;
    generation_from(&row)
}

/// One generation, if it belongs to the authority's Firm.
///
/// # Errors
/// [`ArchiveError::GenerationNotFound`] for a missing generation or one owned
/// by another Firm, so a Firm boundary never discloses another Firm's rows.
pub async fn find(
    pg: &PostgresDb,
    authority: &ArchiveAuthority,
    id: Uuid,
) -> Result<Generation, ArchiveError> {
    let generation = load(pg, id).await?;
    if generation.firm_id != authority.firm_id {
        return Err(ArchiveError::GenerationNotFound);
    }
    Ok(generation)
}

/// Every generation of one Project, newest first.
///
/// # Errors
/// A database error.
pub async fn for_project(
    pg: &PostgresDb,
    authority: &ArchiveAuthority,
    project_id: Uuid,
) -> Result<Vec<Generation>, ArchiveError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GENERATION_COLUMNS} FROM project_archive_generation \
         WHERE firm_id = $1 AND project_id = $2 ORDER BY requested_at DESC, id DESC"
    )))
    .bind(authority.firm_id)
    .bind(project_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(generation_from).collect()
}

/// The newest committed generation of one Project, if any. Not
/// authority-scoped: lifecycle and retention gates read it as a fact.
///
/// # Errors
/// A database error.
pub async fn latest_committed(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<Generation>, ArchiveError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GENERATION_COLUMNS} FROM project_archive_generation \
         WHERE project_id = $1 AND state = 'committed' \
         ORDER BY committed_at DESC, id DESC LIMIT 1"
    )))
    .bind(project_id)
    .fetch_optional(pg.pool())
    .await?;
    row.as_ref().map(generation_from).transpose()
}

/// Every generation not yet committed, oldest first: the durable work queue
/// the scheduled recovery workflow resumes.
///
/// # Errors
/// A database error.
pub async fn unfinished(pg: &PostgresDb, limit: i64) -> Result<Vec<Generation>, ArchiveError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GENERATION_COLUMNS} FROM project_archive_generation \
         WHERE state <> 'committed' ORDER BY requested_at ASC, id ASC LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(generation_from).collect()
}

/// The items of one generation, ordered by path.
///
/// # Errors
/// A database error.
pub async fn items(pg: &PostgresDb, generation_id: Uuid) -> Result<Vec<ArchiveItem>, ArchiveError> {
    let rows = sqlx::query(
        "SELECT path, kind, asset_id, source_key, object_key, sha256, size_bytes, verified_at \
         FROM project_archive_item WHERE generation_id = $1 ORDER BY path",
    )
    .bind(generation_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter()
        .map(|row| {
            Ok(ArchiveItem {
                path: row.try_get("path")?,
                kind: row.try_get("kind")?,
                asset_id: row.try_get("asset_id")?,
                source_key: row.try_get("source_key")?,
                object_key: row.try_get("object_key")?,
                sha256: row.try_get("sha256")?,
                size_bytes: row.try_get("size_bytes")?,
                verified_at: row.try_get("verified_at")?,
            })
        })
        .collect()
}

/// Record a durable request to archive one Project. The same operation key
/// returns the same generation; a key already naming another Project or
/// another source backup is refused.
///
/// A Project with a repository needs the verified Git bundle receipt of that
/// repository: its source and history are part of a complete archive.
///
/// # Errors
/// [`ArchiveError`] for a missing or foreign Project, a malformed key or
/// receipt, or a database error.
pub async fn request(
    stores: &ArchiveStores<'_>,
    authority: &ArchiveAuthority,
    project_id: Uuid,
    operation_key: &str,
    source_backup: Option<SourceBackupReference>,
    now: DateTime<Utc>,
) -> Result<Generation, ArchiveError> {
    if operation_key.is_empty()
        || operation_key.len() > OPERATION_KEY_MAX
        || !operation_key.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(ArchiveError::InvalidOperationKey);
    }
    let project = crate::projects::find_by_id(stores.surreal, project_id)
        .await
        .map_err(store_error)?
        .ok_or(ArchiveError::ProjectNotFound)?;
    let firm_id = project.firm_id.ok_or(ArchiveError::NoFirm)?;
    if firm_id != authority.firm_id {
        return Err(ArchiveError::ProjectNotFound);
    }
    if let Some(backup) = &source_backup {
        backup.validate()?;
    } else if project.repository_url.is_some() {
        return Err(ArchiveError::SourceBackupRequired);
    }
    let backup_json = source_backup
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(store_error)?;
    sqlx::query(
        "INSERT INTO project_archive_generation \
         (id, firm_id, project_id, operation_key, state, requested_by_person_id, requested_at, \
          source_backup) \
         VALUES ($1, $2, $3, $4, 'pending', $5, $6, $7) ON CONFLICT (operation_key) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(firm_id)
    .bind(project_id)
    .bind(operation_key)
    .bind(authority.person_id)
    .bind(now)
    .bind(&backup_json)
    .execute(stores.pg.pool())
    .await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {GENERATION_COLUMNS} FROM project_archive_generation WHERE operation_key = $1"
    )))
    .bind(operation_key)
    .fetch_one(stores.pg.pool())
    .await?;
    let generation = generation_from(&row)?;
    if generation.project_id != project_id
        || generation.firm_id != firm_id
        || generation.source_backup != source_backup
    {
        return Err(ArchiveError::OperationKeyConflict);
    }
    Ok(generation)
}

/// Where one planned item's bytes come from.
#[derive(Debug, Clone)]
enum Source {
    Documents(String),
    Bytes(Vec<u8>),
    Bundle,
}

#[derive(Debug, Clone)]
struct PlannedItem {
    item: ArchiveItem,
    source: Source,
}

#[derive(Debug)]
struct Plan {
    project: Project,
    inventory_sha256: String,
    items: Vec<PlannedItem>,
}

/// Read the Project's current recoverable record and describe every item a
/// complete generation must hold.
async fn plan(stores: &ArchiveStores<'_>, generation: &Generation) -> Result<Plan, ArchiveError> {
    let surreal = stores.surreal;
    let project = crate::projects::find_by_id(surreal, generation.project_id)
        .await
        .map_err(store_error)?
        .ok_or(ArchiveError::ProjectNotFound)?;
    if project.firm_id != Some(generation.firm_id) {
        return Err(ArchiveError::ProjectNotFound);
    }
    if project.repository_url.is_some() && generation.source_backup.is_none() {
        return Err(ArchiveError::SourceBackupRequired);
    }
    let (firm_id, project_id) = (generation.firm_id, generation.project_id);
    let mut items = Vec::new();
    let mut assets: BTreeMap<Uuid, Asset> = BTreeMap::new();
    for asset in crate::assets::for_project_including_deleted(surreal, project_id)
        .await
        .map_err(store_error)?
    {
        if asset.purged_at.is_none() {
            items.push(document_item(
                firm_id,
                project_id,
                kind::DOCUMENT_REVISION,
                format!("documents/{}", asset.id),
                &asset,
            ));
        }
        assets.insert(asset.id, asset);
    }

    let (notations, notation_documents, versions) =
        plan_notations(stores, firm_id, project_id, &mut assets, &mut items).await?;

    let metadata = RestoreMetadata {
        format: FORMAT,
        project: project.clone(),
        assets: assets.into_values().collect(),
        notations,
        notation_documents,
        notation_document_versions: versions,
    };
    let bytes = serde_json::to_vec(&metadata).map_err(store_error)?;
    let sha256 = sha256_hex(&bytes);
    items.push(PlannedItem {
        item: ArchiveItem {
            path: RESTORE_METADATA_PATH.into(),
            kind: kind::RESTORE_METADATA.into(),
            asset_id: None,
            source_key: None,
            object_key: object_key(firm_id, project_id, &sha256),
            size_bytes: size_of(&bytes),
            sha256,
            verified_at: None,
        },
        source: Source::Bytes(bytes),
    });
    if let Some(backup) = &generation.source_backup {
        items.push(PlannedItem {
            item: ArchiveItem {
                path: SOURCE_BACKUP_PATH.into(),
                kind: kind::SOURCE_BACKUP.into(),
                asset_id: None,
                source_key: None,
                object_key: backup.object.clone(),
                sha256: backup.sha256.clone(),
                size_bytes: i64::try_from(backup.size_bytes).unwrap_or(i64::MAX),
                verified_at: None,
            },
            source: Source::Bundle,
        });
    }
    items.sort_by(|left, right| left.item.path.cmp(&right.item.path));
    let inventory = serde_json::to_vec(
        &items
            .iter()
            .map(|planned| &planned.item)
            .collect::<Vec<_>>(),
    )
    .map_err(store_error)?;
    Ok(Plan {
        project,
        inventory_sha256: sha256_hex(&inventory),
        items,
    })
}

/// A Project's Notations, their document identities, and every version.
type NotationRecord = (
    Vec<Notation>,
    Vec<NotationDocument>,
    Vec<NotationDocumentVersion>,
);

/// Plan the Notation rows, their Markdown-source chains, and their source and
/// output bytes.
async fn plan_notations(
    stores: &ArchiveStores<'_>,
    firm_id: Uuid,
    project_id: Uuid,
    assets: &mut BTreeMap<Uuid, Asset>,
    items: &mut Vec<PlannedItem>,
) -> Result<NotationRecord, ArchiveError> {
    let surreal = stores.surreal;
    let mut notations = crate::notations::list_by_project(surreal, project_id)
        .await
        .map_err(store_error)?;
    notations.sort_by_key(|notation| notation.id);
    let mut notation_documents = Vec::new();
    let mut versions = Vec::new();
    for notation in &notations {
        if let Some(document) =
            crate::notation_documents::document_for_notation(surreal, notation.id)
                .await
                .map_err(store_error)?
        {
            let mut source_assets = vec![document.original_asset_id];
            for version in crate::notation_documents::versions_of(surreal, document.id)
                .await
                .map_err(store_error)?
            {
                source_assets.push(version.markdown_asset_id);
                source_assets.push(version.anchor_manifest_asset_id);
                versions.push(version);
            }
            for asset_id in source_assets {
                if assets.contains_key(&asset_id) {
                    continue;
                }
                let asset = crate::assets::find_by_id(surreal, asset_id)
                    .await
                    .map_err(store_error)?
                    .ok_or_else(|| ArchiveError::SourceMissing(format!("asset {asset_id}")))?;
                if asset.project_id.is_some() {
                    // Another Project's document is never part of this archive.
                    return Err(ArchiveError::SourceMissing(format!("asset {asset_id}")));
                }
                items.push(document_item(
                    firm_id,
                    project_id,
                    kind::NOTATION_SOURCE,
                    format!("notation-sources/{asset_id}"),
                    &asset,
                ));
                assets.insert(asset_id, asset);
            }
            notation_documents.push(document);
        }
        plan_notation_outputs(stores, firm_id, project_id, notation.id, items).await?;
    }
    notation_documents.sort_by_key(|document| document.id);
    versions.sort_by_key(|version| version.id);

    Ok((notations, notation_documents, versions))
}

/// Plan whichever rendered, signed, and certificate PDFs a Notation has.
async fn plan_notation_outputs(
    stores: &ArchiveStores<'_>,
    firm_id: Uuid,
    project_id: Uuid,
    notation_id: Uuid,
    items: &mut Vec<PlannedItem>,
) -> Result<(), ArchiveError> {
    for (name, key) in [
        (
            "document.pdf",
            crate::notations::document_pdf_storage_key(notation_id),
        ),
        (
            "signed.pdf",
            crate::notations::signed_document_storage_key(notation_id),
        ),
        (
            "certificate.pdf",
            crate::notations::certificate_of_completion_storage_key(notation_id),
        ),
    ] {
        match stores.documents.get(&key).await {
            Ok(object) => {
                let sha256 = sha256_hex(&object.bytes);
                items.push(PlannedItem {
                    item: ArchiveItem {
                        path: format!("notation-outputs/{notation_id}/{name}"),
                        kind: kind::NOTATION_OUTPUT.into(),
                        asset_id: None,
                        source_key: Some(key.clone()),
                        object_key: object_key(firm_id, project_id, &sha256),
                        size_bytes: size_of(&object.bytes),
                        sha256,
                        verified_at: None,
                    },
                    source: Source::Documents(key),
                });
            }
            Err(StorageError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn document_item(
    firm_id: Uuid,
    project_id: Uuid,
    item_kind: &str,
    path: String,
    asset: &Asset,
) -> PlannedItem {
    PlannedItem {
        item: ArchiveItem {
            path,
            kind: item_kind.into(),
            asset_id: Some(asset.id),
            source_key: Some(asset.storage_key.clone()),
            object_key: object_key(firm_id, project_id, &asset.sha256_hex),
            sha256: asset.sha256_hex.clone(),
            size_bytes: asset.byte_size,
            verified_at: None,
        },
        source: Source::Documents(asset.storage_key.clone()),
    }
}

/// Replace a generation's planned items with `plan` when its inventory
/// changed. Verified items whose path, object, and digest are unchanged keep
/// their checkpoint. The cutoff moves only when the inventory does.
async fn apply_plan(
    pg: &PostgresDb,
    generation: &Generation,
    plan: &Plan,
    now: DateTime<Utc>,
) -> Result<(), ArchiveError> {
    if generation.inventory_sha256.as_deref() == Some(plan.inventory_sha256.as_str()) {
        return Ok(());
    }
    let mut tx = pg.pool().begin().await?;
    let current: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT path, object_key, sha256, size_bytes FROM project_archive_item \
         WHERE generation_id = $1",
    )
    .bind(generation.id)
    .fetch_all(&mut *tx)
    .await?;
    let wanted: BTreeMap<&str, &ArchiveItem> = plan
        .items
        .iter()
        .map(|planned| (planned.item.path.as_str(), &planned.item))
        .collect();
    for (path, object, sha, size) in &current {
        let keep = wanted.get(path.as_str()).is_some_and(|item| {
            &item.object_key == object && &item.sha256 == sha && item.size_bytes == *size
        });
        if !keep {
            sqlx::query("DELETE FROM project_archive_item WHERE generation_id = $1 AND path = $2")
                .bind(generation.id)
                .bind(path)
                .execute(&mut *tx)
                .await?;
        }
    }
    for planned in &plan.items {
        let item = &planned.item;
        sqlx::query(
            "INSERT INTO project_archive_item \
             (generation_id, path, kind, asset_id, source_key, object_key, sha256, size_bytes) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (generation_id, path) DO NOTHING",
        )
        .bind(generation.id)
        .bind(&item.path)
        .bind(&item.kind)
        .bind(item.asset_id)
        .bind(&item.source_key)
        .bind(&item.object_key)
        .bind(&item.sha256)
        .bind(item.size_bytes)
        .execute(&mut *tx)
        .await?;
    }
    let updated = sqlx::query(
        "UPDATE project_archive_generation SET state = 'copying', inventory_sha256 = $2, \
         cutoff_at = $3 WHERE id = $1 AND state <> 'committed' \
         AND inventory_sha256 IS NOT DISTINCT FROM $4",
    )
    .bind(generation.id)
    .bind(&plan.inventory_sha256)
    .bind(now)
    .bind(&generation.inventory_sha256)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        // Another runner re-planned or committed first; its plan stands.
        tx.rollback().await?;
        return Ok(());
    }
    tx.commit().await?;
    Ok(())
}

/// Copy and verify one item, then record its checkpoint.
async fn copy_item(
    stores: &ArchiveStores<'_>,
    generation: &Generation,
    planned: &PlannedItem,
    now: DateTime<Utc>,
) -> Result<(), ArchiveError> {
    let generation_id = generation.id;
    let item = &planned.item;
    let bytes = match &planned.source {
        Source::Bundle => {
            let backup = generation
                .source_backup
                .as_ref()
                .ok_or(ArchiveError::SourceBackupRequired)?;
            verify_source_backup(stores.source_backups, backup, true).await?;
            return checkpoint(stores.pg, generation_id, item, now).await;
        }
        Source::Bytes(bytes) => bytes.clone(),
        Source::Documents(key) => {
            let object = match stores.documents.get(key).await {
                Ok(object) => object,
                Err(StorageError::NotFound(_)) => {
                    return Err(ArchiveError::SourceMissing(item.path.clone()))
                }
                Err(error) => return Err(error.into()),
            };
            if size_of(&object.bytes) != item.size_bytes || sha256_hex(&object.bytes) != item.sha256
            {
                return Err(ArchiveError::SourceCorrupt(item.path.clone()));
            }
            object.bytes
        }
    };
    let content_type = if item.kind == kind::RESTORE_METADATA {
        JSON_CONTENT_TYPE
    } else {
        OBJECT_CONTENT_TYPE
    };
    write_verified(
        stores.archive,
        &item.object_key,
        &bytes,
        content_type,
        &item.path,
    )
    .await?;
    checkpoint(stores.pg, generation_id, item, now).await
}

/// Ensure `key` holds exactly `bytes`: an intact existing object is kept, a
/// missing or damaged one is (re)written, and the result is read back.
pub(crate) async fn write_verified(
    storage: &Arc<dyn StorageService>,
    key: &str,
    bytes: &[u8],
    content_type: &str,
    path: &str,
) -> Result<(), ArchiveError> {
    let expected = sha256_hex(bytes);
    let intact = match storage.get(key).await {
        Ok(object) => sha256_hex(&object.bytes) == expected,
        Err(StorageError::NotFound(_)) => false,
        Err(error) => return Err(error.into()),
    };
    if !intact {
        if let Err(error) = storage.put(key, bytes, content_type).await {
            // The provider may have stored the object before failing; the
            // read-back below settles it either way.
            tracing::warn!(%error, %path, "archive write reported an error; reading back");
        }
    }
    match storage.get(key).await {
        Ok(object)
            if object.bytes.len() == bytes.len() && sha256_hex(&object.bytes) == expected =>
        {
            Ok(())
        }
        Ok(_) => Err(ArchiveError::ArchiveCorrupt(path.into())),
        Err(StorageError::NotFound(_)) => Err(ArchiveError::ArchiveMissing(path.into())),
        Err(error) => Err(error.into()),
    }
}

/// Read the Git bundle back from its own lane at the receipt's exact
/// generation and check it against the receipt. `Ok(false)` only when the
/// lane is absent and the caller did not require it.
async fn verify_source_backup(
    storage: Option<&Arc<dyn StorageService>>,
    backup: &SourceBackupReference,
    required: bool,
) -> Result<bool, ArchiveError> {
    let Some(storage) = storage else {
        return if required {
            Err(ArchiveError::SourceBackupStorageMissing)
        } else {
            Ok(false)
        };
    };
    match storage
        .get_generation(&backup.object, backup.generation)
        .await
    {
        Ok(read)
            if read.generation == backup.generation
                && read.object.bytes.len() as u64 == backup.size_bytes
                && sha256_hex(&read.object.bytes) == backup.sha256 =>
        {
            Ok(true)
        }
        Ok(_) => Err(ArchiveError::SourceCorrupt(SOURCE_BACKUP_PATH.into())),
        Err(StorageError::NotFound(_)) => {
            Err(ArchiveError::SourceMissing(SOURCE_BACKUP_PATH.into()))
        }
        Err(error) => Err(error.into()),
    }
}

async fn checkpoint(
    pg: &PostgresDb,
    generation_id: Uuid,
    item: &ArchiveItem,
    now: DateTime<Utc>,
) -> Result<(), ArchiveError> {
    sqlx::query(
        "UPDATE project_archive_item SET verified_at = $3 \
         WHERE generation_id = $1 AND path = $2 AND verified_at IS NULL \
         AND sha256 = $4 AND object_key = $5",
    )
    .bind(generation_id)
    .bind(&item.path)
    .bind(now)
    .bind(&item.sha256)
    .bind(&item.object_key)
    .execute(pg.pool())
    .await?;
    Ok(())
}

/// Run or resume one archive generation until it commits.
///
/// Safe to call any number of times, concurrently, and after any crash: every
/// step is an idempotent copy-and-verify keyed by content and path, and the
/// commit is a single conditional update. A committed generation is returned
/// unchanged.
///
/// # Errors
/// [`ArchiveError`] when an item's source is missing or corrupt, an archived
/// object cannot be verified, the Project keeps changing, or storage or the
/// database fail. The generation stays uncommitted and records the error.
pub async fn run(
    stores: &ArchiveStores<'_>,
    authority: &ArchiveAuthority,
    generation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Generation, ArchiveError> {
    let generation = find(stores.pg, authority, generation_id).await?;
    resume(stores, generation, now).await
}

/// [`run`] for the durable recovery workflow, which acts on the generation's
/// own recorded Firm authority rather than a live session.
///
/// # Errors
/// As [`run`].
pub async fn resume_unattended(
    stores: &ArchiveStores<'_>,
    generation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Generation, ArchiveError> {
    let generation = load(stores.pg, generation_id).await?;
    resume(stores, generation, now).await
}

async fn resume(
    stores: &ArchiveStores<'_>,
    generation: Generation,
    now: DateTime<Utc>,
) -> Result<Generation, ArchiveError> {
    if generation.state == GenerationState::Committed {
        return Ok(generation);
    }
    sqlx::query(
        "UPDATE project_archive_generation SET attempts = attempts + 1 \
         WHERE id = $1 AND state <> 'committed'",
    )
    .bind(generation.id)
    .execute(stores.pg.pool())
    .await?;
    match advance(stores, generation.id, now).await {
        Ok(generation) => Ok(generation),
        Err(error) => {
            let message = error.to_string();
            sqlx::query(
                "UPDATE project_archive_generation SET last_error = $2 \
                 WHERE id = $1 AND state <> 'committed'",
            )
            .bind(generation.id)
            .bind(&message)
            .execute(stores.pg.pool())
            .await?;
            Err(error)
        }
    }
}

async fn advance(
    stores: &ArchiveStores<'_>,
    generation_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Generation, ArchiveError> {
    for _ in 0..MAX_PLANS {
        let generation = load(stores.pg, generation_id).await?;
        if generation.state == GenerationState::Committed {
            return Ok(generation);
        }
        let planned = plan(stores, &generation).await?;
        apply_plan(stores.pg, &generation, &planned, now).await?;
        let checkpoints: BTreeSet<String> = items(stores.pg, generation_id)
            .await?
            .into_iter()
            .filter(|item| item.verified_at.is_some())
            .map(|item| item.path)
            .collect();
        for item in &planned.items {
            if !checkpoints.contains(&item.item.path) {
                copy_item(stores, &generation, item, now).await?;
            }
        }
        let settled = plan(stores, &generation).await?;
        if settled.inventory_sha256 != planned.inventory_sha256 {
            continue;
        }
        if let Some(generation) = commit(stores, generation_id, &settled, now).await? {
            return Ok(generation);
        }
    }
    Err(ArchiveError::Unstable)
}

/// Write the manifest and commit, or return `None` when the stored plan no
/// longer matches `plan` and the caller must re-plan.
async fn commit(
    stores: &ArchiveStores<'_>,
    generation_id: Uuid,
    plan: &Plan,
    now: DateTime<Utc>,
) -> Result<Option<Generation>, ArchiveError> {
    let generation = load(stores.pg, generation_id).await?;
    if generation.state == GenerationState::Committed {
        return Ok(Some(generation));
    }
    if generation.inventory_sha256.as_deref() != Some(plan.inventory_sha256.as_str()) {
        return Ok(None);
    }
    let rows = items(stores.pg, generation_id).await?;
    let expected: Vec<&ArchiveItem> = plan.items.iter().map(|planned| &planned.item).collect();
    if rows.len() != expected.len()
        || rows.iter().zip(&expected).any(|(row, item)| {
            row.path != item.path || row.sha256 != item.sha256 || row.object_key != item.object_key
        })
    {
        return Ok(None);
    }
    if let Some(unverified) = rows.iter().find(|row| row.verified_at.is_none()) {
        return Err(ArchiveError::ArchiveMissing(unverified.path.clone()));
    }
    for row in rows.iter().filter(|row| row.kind != kind::SOURCE_BACKUP) {
        match stores.archive.head(&row.object_key).await? {
            Some(head) if i64::try_from(head.size_bytes).ok() == Some(row.size_bytes) => {}
            Some(_) => return Err(ArchiveError::ArchiveCorrupt(row.path.clone())),
            None => return Err(ArchiveError::ArchiveMissing(row.path.clone())),
        }
    }
    let cutoff_at = generation
        .cutoff_at
        .ok_or_else(|| ArchiveError::Database("planned generation has no cutoff".into()))?;
    let manifest = Manifest {
        format: FORMAT,
        generation_id,
        operation_key: generation.operation_key.clone(),
        firm_id: generation.firm_id,
        project_id: generation.project_id,
        project_code: plan.project.code.clone(),
        cutoff_at,
        inventory_sha256: plan.inventory_sha256.clone(),
        source_backup: generation.source_backup.clone(),
        items: rows,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(store_error)?;
    let sha256 = sha256_hex(&bytes);
    let key = manifest_key(&generation, &sha256);
    write_verified(stores.archive, &key, &bytes, JSON_CONTENT_TYPE, "manifest").await?;
    let committed = sqlx::query(
        "UPDATE project_archive_generation SET state = 'committed', manifest_key = $2, \
         manifest_sha256 = $3, manifest_size_bytes = $4, item_count = $5, committed_at = $6, \
         last_error = NULL \
         WHERE id = $1 AND state = 'copying' AND inventory_sha256 = $7",
    )
    .bind(generation_id)
    .bind(&key)
    .bind(&sha256)
    .bind(size_of(&bytes))
    .bind(i32::try_from(manifest.items.len()).unwrap_or(i32::MAX))
    .bind(now)
    .bind(&plan.inventory_sha256)
    .execute(stores.pg.pool())
    .await?
    .rows_affected();
    let generation = load(stores.pg, generation_id).await?;
    if committed == 1 || generation.state == GenerationState::Committed {
        Ok(Some(generation))
    } else {
        Ok(None)
    }
}

/// Read and verify a committed generation's manifest against its row.
async fn read_manifest(
    archive: &Arc<dyn StorageService>,
    generation: &Generation,
) -> Result<Manifest, ArchiveError> {
    if generation.state != GenerationState::Committed {
        return Err(ArchiveError::NotCommitted);
    }
    let (Some(key), Some(sha256)) = (&generation.manifest_key, &generation.manifest_sha256) else {
        return Err(ArchiveError::NotCommitted);
    };
    let bytes = match archive.get(key).await {
        Ok(object) => object.bytes,
        Err(StorageError::NotFound(_)) => {
            return Err(ArchiveError::ArchiveMissing("manifest".into()))
        }
        Err(error) => return Err(error.into()),
    };
    if &sha256_hex(&bytes) != sha256 || Some(size_of(&bytes)) != generation.manifest_size_bytes {
        return Err(ArchiveError::ArchiveCorrupt("manifest".into()));
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| ArchiveError::ManifestMismatch)?;
    if manifest.format != FORMAT
        || manifest.generation_id != generation.id
        || manifest.firm_id != generation.firm_id
        || manifest.project_id != generation.project_id
        || Some(&manifest.inventory_sha256) != generation.inventory_sha256.as_ref()
        || i32::try_from(manifest.items.len()).ok() != generation.item_count
        || manifest.source_backup != generation.source_backup
    {
        return Err(ArchiveError::ManifestMismatch);
    }
    Ok(manifest)
}

async fn read_item(
    archive: &Arc<dyn StorageService>,
    item: &ArchiveItem,
) -> Result<Vec<u8>, ArchiveError> {
    let bytes = match archive.get(&item.object_key).await {
        Ok(object) => object.bytes,
        Err(StorageError::NotFound(_)) => {
            return Err(ArchiveError::ArchiveMissing(item.path.clone()))
        }
        Err(error) => return Err(error.into()),
    };
    if size_of(&bytes) != item.size_bytes || sha256_hex(&bytes) != item.sha256 {
        return Err(ArchiveError::ArchiveCorrupt(item.path.clone()));
    }
    Ok(bytes)
}

/// What [`verify`] read back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    /// Items whose archived bytes match their size and digest.
    pub verified: usize,
    /// Whether the source backup was read back from its own lane.
    pub source_backup_verified: bool,
    /// Paths whose bytes the governed retention purge removed afterward.
    pub purged: Vec<String>,
    /// Paths that are missing or damaged.
    pub failures: Vec<String>,
}

/// Re-read every byte of a committed generation and compare it with the
/// manifest. Nothing is written.
///
/// # Errors
/// [`ArchiveError`] when the generation is missing, foreign, uncommitted, or
/// its manifest is missing or does not match.
pub async fn verify(
    pg: &PostgresDb,
    archive: &Arc<dyn StorageService>,
    source_backups: Option<&Arc<dyn StorageService>>,
    authority: &ArchiveAuthority,
    generation_id: Uuid,
) -> Result<VerifyReport, ArchiveError> {
    let generation = find(pg, authority, generation_id).await?;
    let manifest = read_manifest(archive, &generation).await?;
    let purged = crate::document_retention::purged_digests(pg, generation.project_id)
        .await
        .map_err(store_error)?;
    let mut report = VerifyReport::default();
    for item in &manifest.items {
        if item.kind == kind::DOCUMENT_REVISION && purged.contains_key(&item.sha256) {
            report.purged.push(item.path.clone());
            continue;
        }
        let outcome = if item.kind == kind::SOURCE_BACKUP {
            let backup = manifest
                .source_backup
                .as_ref()
                .ok_or(ArchiveError::ManifestMismatch)?;
            verify_source_backup(source_backups, backup, false)
                .await
                .map(|read| report.source_backup_verified = read)
        } else {
            read_item(archive, item).await.map(drop)
        };
        match outcome {
            Ok(()) => report.verified += 1,
            Err(
                ArchiveError::ArchiveMissing(path)
                | ArchiveError::ArchiveCorrupt(path)
                | ArchiveError::SourceMissing(path)
                | ArchiveError::SourceCorrupt(path),
            ) => report.failures.push(path),
            Err(error) => return Err(error),
        }
    }
    Ok(report)
}

/// The store a generation is restored into. It may be a fresh, isolated
/// deployment or the live one; either way nothing different is overwritten.
pub struct RestoreTarget<'a> {
    pub surreal: &'a SurrealDb,
    pub documents: &'a Arc<dyn StorageService>,
}

/// What one restore wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    pub rows_created: usize,
    pub rows_identical: usize,
    pub objects_written: usize,
    pub objects_present: usize,
    /// The Git bundle reference to restore the repository from with
    /// `navigator ops forgejo bundle restore`.
    pub source_backup: Option<SourceBackupReference>,
}

/// Restore one committed generation into `target`.
///
/// Every archived object is read back and verified before anything is
/// written. Bytes return to their original storage keys and rows to their
/// original ids. The target must already hold the Project's Firm and client
/// Entity; a restore never creates unrelated Firm records. An existing row or
/// object that differs stops the restore with [`ArchiveError::RestoreConflict`]
/// rather than being replaced, and a rerun after any interruption converges.
///
/// # Errors
/// [`ArchiveError`] for a missing, foreign, or uncommitted generation, a
/// damaged object, a missing prerequisite, a conflict, or a store failure.
pub async fn restore(
    pg: &PostgresDb,
    archive: &Arc<dyn StorageService>,
    authority: &ArchiveAuthority,
    generation_id: Uuid,
    target: &RestoreTarget<'_>,
) -> Result<RestoreReport, ArchiveError> {
    let generation = find(pg, authority, generation_id).await?;
    let manifest = read_manifest(archive, &generation).await?;
    let purged = crate::document_retention::purged_digests(pg, generation.project_id)
        .await
        .map_err(store_error)?;
    let mut bytes_by_digest: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut metadata = None;
    for item in manifest.items.iter().filter(|item| {
        item.kind != kind::SOURCE_BACKUP
            && !(item.kind == kind::DOCUMENT_REVISION && purged.contains_key(&item.sha256))
    }) {
        let bytes = read_item(archive, item).await?;
        if item.kind == kind::RESTORE_METADATA {
            metadata = Some(
                serde_json::from_slice::<RestoreMetadata>(&bytes)
                    .map_err(|_| ArchiveError::ManifestMismatch)?,
            );
        }
        bytes_by_digest.insert(item.sha256.clone(), bytes);
    }
    let mut metadata = metadata.ok_or(ArchiveError::ManifestMismatch)?;
    // A revision the retention purge removed after this generation committed
    // comes back as the tombstone the purge left, never with bytes.
    for asset in &mut metadata.assets {
        if asset.purged_at.is_none() && asset.project_id.is_some() {
            if let Some(at) = purged.get(&asset.sha256_hex) {
                asset.purged_at = Some(*at);
            }
        }
    }
    if metadata.format != FORMAT
        || metadata.project.id != generation.project_id
        || metadata.project.firm_id != Some(generation.firm_id)
    {
        return Err(ArchiveError::ManifestMismatch);
    }
    if crate::firms::find_by_id(target.surreal, generation.firm_id)
        .await
        .map_err(store_error)?
        .is_none()
    {
        return Err(ArchiveError::RestorePrerequisite("the Project's Firm"));
    }
    if crate::entities::find_by_id(target.surreal, metadata.project.entity_id)
        .await
        .map_err(store_error)?
        .is_none()
    {
        return Err(ArchiveError::RestorePrerequisite(
            "the Project's client Entity",
        ));
    }

    let mut report = RestoreReport {
        source_backup: manifest.source_backup.clone(),
        ..RestoreReport::default()
    };
    restore_objects(&manifest, &metadata, &bytes_by_digest, target, &mut report).await?;
    restore_rows(&metadata, target, &mut report).await?;
    Ok(report)
}

/// Return every archived object to its original documents-storage key.
async fn restore_objects(
    manifest: &Manifest,
    metadata: &RestoreMetadata,
    bytes_by_digest: &BTreeMap<String, Vec<u8>>,
    target: &RestoreTarget<'_>,
    report: &mut RestoreReport,
) -> Result<(), ArchiveError> {
    let mut objects: BTreeMap<String, String> = BTreeMap::new();
    for item in &manifest.items {
        if matches!(item.kind.as_str(), kind::NOTATION_OUTPUT) {
            if let Some(key) = &item.source_key {
                objects.insert(key.clone(), item.sha256.clone());
            }
        }
    }
    for asset in metadata
        .assets
        .iter()
        .filter(|asset| asset.purged_at.is_none())
    {
        for key in std::iter::once(&asset.storage_key).chain(asset.secondary_storage_key.as_ref()) {
            objects.insert(key.clone(), asset.sha256_hex.clone());
        }
    }
    for (key, sha256) in &objects {
        let bytes = bytes_by_digest
            .get(sha256)
            .ok_or_else(|| ArchiveError::ArchiveMissing(key.clone()))?;
        match target.documents.get(key).await {
            Ok(existing) if sha256_hex(&existing.bytes) == *sha256 => {
                report.objects_present += 1;
                continue;
            }
            Ok(_) => return Err(ArchiveError::RestoreConflict(format!("object {key}"))),
            Err(StorageError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        }
        write_verified(target.documents, key, bytes, OBJECT_CONTENT_TYPE, key).await?;
        report.objects_written += 1;
    }

    Ok(())
}

/// Write every archived row back under its original id.
async fn restore_rows(
    metadata: &RestoreMetadata,
    target: &RestoreTarget<'_>,
    report: &mut RestoreReport,
) -> Result<(), ArchiveError> {
    let mut tally = |outcome: RowRestore, what: String| match outcome {
        RowRestore::Created => {
            report.rows_created += 1;
            Ok(())
        }
        RowRestore::Identical => {
            report.rows_identical += 1;
            Ok(())
        }
        RowRestore::Conflict => Err(ArchiveError::RestoreConflict(what)),
    };
    tally(
        crate::projects::restore_archived(target.surreal, &metadata.project)
            .await
            .map_err(store_error)?,
        format!("project {}", metadata.project.id),
    )?;
    for asset in &metadata.assets {
        tally(
            crate::assets::restore_archived(target.surreal, asset)
                .await
                .map_err(store_error)?,
            format!("asset {}", asset.id),
        )?;
    }
    for notation in &metadata.notations {
        tally(
            crate::notations::restore_archived(target.surreal, notation)
                .await
                .map_err(store_error)?,
            format!("notation {}", notation.id),
        )?;
    }
    for document in &metadata.notation_documents {
        tally(
            crate::notation_documents::restore_archived_document(target.surreal, document)
                .await
                .map_err(store_error)?,
            format!("notation document {}", document.id),
        )?;
    }
    for version in &metadata.notation_document_versions {
        tally(
            crate::notation_documents::restore_archived_version(target.surreal, version)
                .await
                .map_err(store_error)?,
            format!("notation document version {}", version.id),
        )?;
    }
    Ok(())
}

/// Whether some committed generation of `project_id` holds verified bytes with
/// this digest. The retention purge and active-copy cleanup read this before
/// they remove an active reference.
///
/// # Errors
/// A database error.
pub async fn digest_is_archived(
    pg: &PostgresDb,
    project_id: Uuid,
    sha256: &str,
) -> Result<bool, ArchiveError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM project_archive_item i \
         JOIN project_archive_generation g ON g.id = i.generation_id \
         WHERE g.project_id = $1 AND g.state = 'committed' AND i.sha256 = $2 \
         AND i.verified_at IS NOT NULL AND i.kind <> 'source_backup')",
    )
    .bind(project_id)
    .bind(sha256)
    .fetch_one(pg.pool())
    .await?)
}

/// A storage-lane health probe: write, read back, and verify one small object
/// under `health/`. Fails closed when the lane is unreachable or returns
/// different bytes.
///
/// # Errors
/// [`ArchiveError`] when the round trip fails.
pub async fn probe(archive: &Arc<dyn StorageService>) -> Result<(), ArchiveError> {
    let bytes = b"navigator archived-projects probe";
    write_verified(
        archive,
        &format!("health/{}", sha256_hex(bytes)),
        bytes,
        OBJECT_CONTENT_TYPE,
        "health probe",
    )
    .await
}
