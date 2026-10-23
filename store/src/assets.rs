//! `store::assets` — the content-addressed lane for the `asset` table,
//! and every query against it.
//!
//! # This table lives in SurrealDB
//!
//! `assets` moved with wave five of #1093 (ENG-121), together with
//! [`crate::templates`], because `templates.asset_id` couples them:
//! `save_version` folds that id into the tuple deciding whether a template
//! version changed, so porting one without the other would leave the lane's
//! version identity in one engine and the bytes it names in the other.
//!
//! # Two shapes, one table
//!
//! [`crate::documents::ingest_bytes`] writes a **document asset** — a
//! matter artifact carrying `filename`/`kind`/provenance. This module is
//! the lower-level seam for a **bare content asset** — a template body or
//! any non-document artifact that wants only the byte pointer: sha-dedup
//! the bytes, write them to object storage at `blobs/<sha>` (an opaque,
//! content-addressed key), and insert/reuse a bare `asset` row.
//!
//! # Engine facts this module is shaped around
//!
//! **The key-value layer is optimistic, so a write can lose a race.**
//! [`writing`] re-runs the statement under the crate's one retry policy,
//! [`crate::surreal::retry`]. The seed and `cli import` both ingest template
//! bodies on every boot, and the cucumber suite shares one engine across
//! concurrent scenarios, so idempotence under contention is the contract.
//!
//! **A multi-statement query is not one transaction.** A dedup probe and a
//! row insert in one query are two statements; see
//! [`crate::documents::ingest_bytes_as`] for what carries that guarantee
//! now.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "asset";

/// One asset — the byte pointer, plus document metadata when the row is a
/// matter document.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`AssetRow`] is the seam that turns it into (and back out of) what the
/// SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    pub id: Uuid,
    /// Object-storage key. Document assets use
    /// `projects/<code>/documents/<sha>`; bare content uses `blobs/<sha>`.
    /// It is not unique: compatible asset rows may point at one
    /// content-addressed object.
    pub storage_key: String,
    /// A second object-storage key holding the **same bytes**, written
    /// outside content-addressing — a generated PDF's notation key
    /// (`notations/<id>/document.pdf`). Recorded so a governed expunge
    /// deletes every copy; `None` for the common single-copy asset.
    pub secondary_storage_key: Option<String>,
    pub content_type: String,
    pub byte_size: i64,
    /// Lowercase hex SHA-256 of the byte content — the dedup key.
    pub sha256_hex: String,
    /// The matter this document belongs to; `None` for a bare content
    /// asset.
    pub project_id: Option<Uuid>,
    pub filename: Option<String>,
    pub kind: Option<String>,
    /// Inbound channel — `upload`, `email`, `generated`.
    pub source: Option<String>,
    pub received_at: Option<String>,
    pub description: Option<String>,
    /// `internal` (lawyer-only) or `client`. See
    /// [`crate::documents::visibility`].
    pub visibility: String,
    /// The document identity within a Project. `None` is a one-off
    /// artifact; `Some` makes this row one **revision** of a living
    /// document, and `(project_id, slug)` is deliberately non-unique.
    pub slug: Option<String>,
    /// Publication stamp, distinct from `received_at`. Display and sort
    /// metadata only — insertion order, not this field, decides which
    /// revision is current.
    pub published_at: Option<String>,
    /// Inbound-mail provenance (ENG-517), unset for every other ingest lane.
    /// The `email_conversation_message.id` an attachment was filed from — a
    /// hand-download loses this and the three fields below it.
    pub source_message_id: Option<String>,
    pub source_sender: Option<String>,
    pub source_received_at: Option<String>,
    pub source_subject: Option<String>,
    /// Free-form JSON carrying per-kind detail. The ported JSONB column;
    /// the schema types it `any`, which is what lets it hold an object, a
    /// nested object, or an array. Validators belong in the `rules` crate
    /// per the S103 discipline, never in a database constraint.
    pub metadata: Option<Json>,
    /// Source revision for a generated transcript, if this row derives from
    /// another Project document. Optional for rows written before LAW-76.
    pub derived_from: Option<Json>,
    /// `machine` or `proofread` when transcript quality is known; absent for
    /// legacy and user-authored transcripts without an explicit assertion.
    pub transcript_quality: Option<String>,
    /// How an executed document was signed, when that was recorded at upload.
    pub signing: Option<crate::documents::Signing>,
    /// The notarial acknowledgments recorded at upload; empty when none were.
    pub notarization: Vec<crate::documents::Notarization>,
    /// When a lawyer soft-deleted this revision's document. Every revision
    /// of the slug chain carries the same stamp. `None` is live.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    /// `deleted_at` plus [`RETENTION_DAYS`]: the first moment the purge may
    /// expunge the chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<DateTime<Utc>>,
    /// The lawyer-tier person who deleted the document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_by_person_id: Option<Uuid>,
    /// The source-repository commit whose pointer removal asked for the
    /// deletion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_commit_sha: Option<String>,
    /// Set while a Legal Hold keeps the chain out of the purge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legal_hold_at: Option<DateTime<Utc>>,
    /// When the governed expunge removed the chain's bytes. The row stays as
    /// the tombstone the expunge record points at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purged_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One lawyer-facing document identity and its insertion-ordered revision
/// history. A `None` slug represents a one-off asset, so `revisions` is empty
/// and `current` is that asset itself.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetHistory {
    pub slug: Option<String>,
    pub current: Asset,
    /// Slugged revisions, newest first. The first entry is always `current`.
    pub revisions: Vec<Asset>,
}

/// The row as the engine reads and writes it. Separate from [`Asset`]
/// because the SDK owns its own `RecordId` and `Datetime`, and the
/// conversion belongs at this seam rather than in every caller.
#[derive(SurrealValue)]
struct AssetRow {
    id: surrealdb::types::RecordId,
    storage_key: String,
    secondary_storage_key: Option<String>,
    content_type: String,
    byte_size: i64,
    sha256_hex: String,
    project_id: Option<surrealdb::types::RecordId>,
    filename: Option<String>,
    kind: Option<String>,
    source: Option<String>,
    received_at: Option<String>,
    description: Option<String>,
    visibility: String,
    slug: Option<String>,
    published_at: Option<String>,
    source_message_id: Option<String>,
    source_sender: Option<String>,
    source_received_at: Option<String>,
    source_subject: Option<String>,
    metadata: Option<Json>,
    derived_from: Option<Json>,
    transcript_quality: Option<String>,
    signing: Option<Json>,
    notarization: Option<Json>,
    deleted_at: Option<surrealdb::types::Datetime>,
    purge_after: Option<surrealdb::types::Datetime>,
    deleted_by_person_id: Option<surrealdb::types::RecordId>,
    deleted_commit_sha: Option<String>,
    legal_hold_at: Option<surrealdb::types::Datetime>,
    purged_at: Option<surrealdb::types::Datetime>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl AssetRow {
    /// `None` when the record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_asset(self) -> Option<Asset> {
        Some(Asset {
            id: record_uuid(&self.id)?,
            storage_key: self.storage_key,
            secondary_storage_key: self.secondary_storage_key,
            content_type: self.content_type,
            byte_size: self.byte_size,
            sha256_hex: self.sha256_hex,
            // A link this module could not have written reads back as
            // "unscoped" rather than failing the whole row: the byte
            // pointer is still usable, and the project scope is what the
            // caller's own ACL already decided.
            project_id: self.project_id.as_ref().and_then(record_uuid),
            filename: self.filename,
            kind: self.kind,
            source: self.source,
            received_at: self.received_at,
            description: self.description,
            visibility: self.visibility,
            slug: self.slug,
            published_at: self.published_at,
            source_message_id: self.source_message_id,
            source_sender: self.source_sender,
            source_received_at: self.source_received_at,
            source_subject: self.source_subject,
            metadata: self.metadata,
            derived_from: self.derived_from,
            transcript_quality: self.transcript_quality,
            // A stored value this build cannot read reads back as unrecorded
            // rather than failing the whole row.
            signing: self
                .signing
                .and_then(|signing| serde_json::from_value(signing).ok()),
            notarization: self
                .notarization
                .and_then(|notarization| serde_json::from_value(notarization).ok())
                .unwrap_or_default(),
            deleted_at: self.deleted_at.map(Into::into),
            purge_after: self.purge_after.map(Into::into),
            deleted_by_person_id: self.deleted_by_person_id.as_ref().and_then(record_uuid),
            deleted_commit_sha: self.deleted_commit_sha,
            legal_hold_at: self.legal_hold_at.map(Into::into),
            purged_at: self.purged_at.map(Into::into),
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new field cannot reach [`AssetRow`] from only one query.
pub(crate) const SELECT: &str = "id, storage_key, secondary_storage_key, content_type, byte_size, \
     sha256_hex, project_id, filename, kind, source, received_at, description, visibility, slug, \
     published_at, source_message_id, source_sender, source_received_at, source_subject, metadata, \
     derived_from, transcript_quality, signing, notarization, deleted_at, purge_after, \
     deleted_by_person_id, deleted_commit_sha, legal_hold_at, purged_at, \
     inserted_at, updated_at";

/// The predicate every default read carries: a soft-deleted document drops
/// out of the matter page and the ordinary lists. The reads that serve a
/// lawyer looking at a deleted document ([`revisions`], [`find_by_id`],
/// [`for_project_including_deleted`]) leave it off.
const LIVE: &str = "deleted_at IS NONE";

/// Errors from [`ingest_content`] / [`fetch`].
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("storage: {0}")]
    Storage(#[from] cloud::StorageError),
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back — see [`AssetRow::into_asset`].
    #[error("writing an asset returned no usable row")]
    WriteReturnedNothing,
}

/// Run a write under the shared retry policy
/// ([`crate::surreal::retry`]), mapping whatever finally comes back to
/// this module's error.
///
/// Only the mapping lives here — for this slice, none at all. How long a
/// lost race is re-run, and which engine conditions count as a lost
/// race, are one policy for the whole crate.
///
/// Still `pub(crate)`: [`crate::documents`] and [`crate::templates`]
/// write these tables too, and reach the policy through this seam.
pub(crate) async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, surrealdb::Error>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await
}

/// Mirror each written asset row into PostgreSQL. A no-op unless the mirror
/// is configured; a failure is logged and left for parity, never returned.
pub(crate) async fn mirror_writes(db: &SurrealDb, ids: &[Uuid]) {
    let keys: Vec<_> = ids
        .iter()
        .copied()
        .map(crate::postgres_mirror::MirrorKey::Asset)
        .collect();
    crate::postgres_mirror::after_writes(db, keys).await;
}

/// Read one asset out of a query response, dropping a row this module
/// could not have written.
fn one(mut response: surrealdb::IndexedResults) -> Result<Option<Asset>, AssetError> {
    let row: Option<AssetRow> = response.take(0)?;
    Ok(row.and_then(AssetRow::into_asset))
}

/// Read every asset out of a query response, in the order the engine
/// returned them.
fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<Asset>, AssetError> {
    let rows: Vec<AssetRow> = response.take(0)?;
    Ok(rows.into_iter().filter_map(AssetRow::into_asset).collect())
}

/// Byte size of the object at `key`, or `None` when the key is absent.
///
/// A metadata read. The bytes stay in storage. Document keys are
/// content-addressed (`projects/<code>/documents/<sha256>`, and `blobs/<sha256>`
/// on a bare content row).
///
/// # Errors
/// [`AssetError::Storage`] when the backend cannot answer.
pub async fn object_head(
    storage: &dyn StorageService,
    key: &str,
) -> Result<Option<u64>, AssetError> {
    Ok(storage.head(key).await?.map(|head| head.size_bytes))
}

/// SHA-256 of the bytes at `key`, or `None` when the key is absent.
///
/// Reads the stored object and hashes those bytes. The integrity route uses
/// this only when a caller asks for a full digest comparison.
///
/// # Errors
/// [`AssetError::Storage`] when the backend cannot read the object.
pub async fn object_sha256(
    storage: &dyn StorageService,
    key: &str,
) -> Result<Option<String>, AssetError> {
    match storage.get(key).await {
        Ok(object) => Ok(Some(sha256_hex(&object.bytes))),
        Err(cloud::StorageError::NotFound(_)) => Ok(None),
        Err(error) => Err(AssetError::Storage(error)),
    }
}

/// The content hash every asset is stored and deduped under.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Resolve an asset by id.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_id(db: &SurrealDb, id: Uuid) -> Result<Option<Asset>, AssetError> {
    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Resolve many assets by id in one round trip — what a listing does after
/// reading a page of rows that reference them, rather than one lookup per
/// row. Ids that match nothing are simply absent.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_ids(db: &SurrealDb, ids: &[Uuid]) -> Result<Vec<Asset>, AssetError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<surrealdb::types::RecordId> =
        ids.iter().map(|id| record_id(TABLE, *id)).collect();
    let response = db
        .query(format!("SELECT {SELECT} FROM $ids"))
        .bind(("ids", keys))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every asset on a Project, newest first — the lawyer document listing.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn for_project(db: &SurrealDb, project_id: Uuid) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project AND {LIVE} ORDER BY id DESC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every asset on a Project, soft-deleted ones included, newest first — what
/// a lawyer reads to find a document to restore.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn for_project_including_deleted(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project ORDER BY id DESC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Read every asset on a Project once, then group slugged rows into
/// insertion-ordered document histories. The query's `id DESC` ordering is
/// the lawyer lens's operative-version rule; `published_at` is display
/// metadata and never participates in selection.
///
/// Assets without a slug remain independent one-off rows and do not acquire a
/// synthetic history. Every returned row came from the Project-scoped query,
/// so a matching slug on another Project cannot enter a group.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn grouped_for_project(
    db: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<AssetHistory>, AssetError> {
    let assets = for_project(db, project_id).await?;
    let mut groups = Vec::new();

    for asset in assets {
        let Some(slug) = asset.slug.as_deref() else {
            groups.push(AssetHistory {
                slug: None,
                current: asset,
                revisions: Vec::new(),
            });
            continue;
        };

        if let Some(group) = groups
            .iter_mut()
            .find(|group: &&mut AssetHistory| group.slug.as_deref() == Some(slug))
        {
            group.revisions.push(asset);
        } else {
            groups.push(AssetHistory {
                slug: Some(slug.to_string()),
                current: asset.clone(),
                revisions: vec![asset],
            });
        }
    }

    for group in &mut groups {
        if let Some(newest) = group.revisions.first() {
            group.current = newest.clone();
        }
    }

    Ok(groups)
}

/// Whether any asset is scoped to this Project — the matter-delete guard.
///
/// # Errors
///
/// [`AssetError::Db`] if the lookup fails.
pub async fn exists_for_project(db: &SurrealDb, project_id: Uuid) -> Result<bool, AssetError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE project_id = $project LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let ids: Vec<surrealdb::types::RecordId> = response.take(0)?;
    Ok(!ids.is_empty())
}

/// The document asset on `project_id` carrying `filename`, if one exists.
///
/// The seeders' natural key. They deliberately dedup on `(project,
/// filename)` rather than the content hash: keying on the hash would
/// insert a second asset whenever a bundled document is re-authored,
/// leaving the matter with duplicate visible versions and an orphaned
/// blob. One filename is one seeded document.
///
/// Newest-first, so a matter that somehow holds two rows under one
/// filename resolves to the one a reader would see rather than an
/// arbitrary one.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_project_and_filename(
    db: &SurrealDb,
    project_id: Uuid,
    filename: &str,
) -> Result<Option<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project AND filename = $filename AND {LIVE} \
             ORDER BY id DESC LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("filename", filename.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// The asset on `project_id` matching `(filename, sha256_hex)` — the
/// re-upload dedup probe. Deliberately narrower than the content hash
/// alone: the same bytes under a different name are a different document.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_filed_copy(
    db: &SurrealDb,
    project_id: Uuid,
    filename: &str,
    sha256_hex: &str,
) -> Result<Option<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project \
             AND filename = $filename AND sha256_hex = $sha AND {LIVE} LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("filename", filename.to_string()))
        .bind(("sha", sha256_hex.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Set `visibility` on **every** row this matter holds for
/// `(filename, sha256_hex)`, and report how many rows changed.
///
/// Every row, not the one a dedup lookup happened to return: a matter can
/// hold several duplicates of one file (uploads filed before dedup
/// existed, concurrent submissions, other ingest paths), and updating only
/// one would leave a duplicate at its old visibility — a re-upload as
/// `internal` could otherwise leave a duplicate still client-visible and
/// its bytes reachable through the client listing and the ZIP export.
///
/// The `visibility != $target` predicate touches only the divergent rows,
/// so this is a no-op when they already agree.
///
/// # Errors
/// [`AssetError::Db`] if the update fails.
pub async fn sync_visibility(
    db: &SurrealDb,
    project_id: Uuid,
    filename: &str,
    sha256_hex: &str,
    visibility: &str,
) -> Result<usize, AssetError> {
    let mut response = writing(|| {
        db.query(format!(
            "UPDATE {TABLE} SET visibility = $visibility, updated_at = time::now() \
             WHERE project_id = $project AND filename = $filename \
             AND sha256_hex = $sha AND visibility != $visibility \
             RETURN id"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("filename", filename.to_string()))
        .bind(("sha", sha256_hex.to_string()))
        .bind(("visibility", visibility.to_string()))
    })
    .await?;
    let changed: Vec<surrealdb::types::RecordId> = response.take((0, "id"))?;
    let ids: Vec<Uuid> = changed.iter().filter_map(record_uuid).collect();
    mirror_writes(db, &ids).await;
    Ok(changed.len())
}

/// The newest asset on `project_id` carrying `kind`, or `None`.
///
/// Newest by insertion order (`id` descending, UUIDv7), the same ordering
/// [`current`] uses — not `inserted_at`, so the two cannot disagree.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn latest_of_kind(
    db: &SurrealDb,
    project_id: Uuid,
    kind: &str,
) -> Result<Option<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project AND kind = $kind \
             AND {LIVE} ORDER BY id DESC LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("kind", kind.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Whether any asset on a matter **other than** `project_id` points at
/// `storage_key` — through either the primary or the secondary key.
///
/// The governed-expunge guard. A row with no `project_id` deliberately
/// does **not** count: it is unattached, not another matter, and treating
/// it as a referent would let a stray row defeat a privilege clawback,
/// which is the failure this primitive exists to prevent.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn referenced_by_another_project(
    db: &SurrealDb,
    project_id: Uuid,
    storage_key: &str,
) -> Result<bool, AssetError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} \
             WHERE (storage_key = $key OR secondary_storage_key = $key) \
             AND project_id != NONE AND project_id != $project LIMIT 1"
        ))
        .bind(("key", storage_key.to_string()))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let found: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(found.is_some())
}

/// Whether an unpurged row other than those in `excluding` still points at
/// `storage_key`, on any Project. A retention purge or active-copy cleanup
/// keeps an object any such row needs, including a sibling chain on the same
/// matter that shares the content-addressed bytes.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn storage_key_needed_elsewhere(
    db: &SurrealDb,
    storage_key: &str,
    excluding: &[Uuid],
) -> Result<bool, AssetError> {
    let excluded: Vec<surrealdb::types::RecordId> =
        excluding.iter().map(|id| record_id(TABLE, *id)).collect();
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} \
             WHERE (storage_key = $key OR secondary_storage_key = $key) \
             AND purged_at IS NONE AND id NOT IN $excluded LIMIT 1"
        ))
        .bind(("key", storage_key.to_string()))
        .bind(("excluded", excluded))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let found: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(found.is_some())
}

/// Whether an unpurged row on `project_id` other than those in `excluding`
/// holds bytes with this digest.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn digest_needed_elsewhere(
    db: &SurrealDb,
    project_id: Uuid,
    sha256_hex: &str,
    excluding: &[Uuid],
) -> Result<bool, AssetError> {
    let excluded: Vec<surrealdb::types::RecordId> =
        excluding.iter().map(|id| record_id(TABLE, *id)).collect();
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} \
             WHERE project_id = $project AND sha256_hex = $sha \
             AND purged_at IS NONE AND id NOT IN $excluded LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("sha", sha256_hex.to_string()))
        .bind(("excluded", excluded))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let found: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(found.is_some())
}

/// Every soft-deleted row the purge has not removed, oldest first, at most
/// `limit`: the chains the retention lane archives.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn soft_deleted_unpurged(db: &SurrealDb, limit: usize) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE deleted_at IS NOT NONE \
             AND purged_at IS NONE ORDER BY id ASC LIMIT $limit"
        ))
        .bind(("limit", i64::try_from(limit).unwrap_or(i64::MAX)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every revision of the chain `asset` belongs to, soft-deleted or not.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn chain(db: &SurrealDb, asset: &Asset) -> Result<Vec<Asset>, AssetError> {
    chain_of(db, asset).await
}

/// How many assets are filed across `project_ids`, in one round trip —
/// the web app's projects-dashboard KPI tile, which needs the number and none of the
/// rows. An empty slice never reaches the engine.
///
/// # Errors
/// [`AssetError::Db`] if the count fails.
pub async fn count_for_projects(db: &SurrealDb, project_ids: &[Uuid]) -> Result<usize, AssetError> {
    if project_ids.is_empty() {
        return Ok(0);
    }
    let keys: Vec<surrealdb::types::RecordId> = project_ids
        .iter()
        .map(|id| record_id(crate::projects::PROJECT_TABLE, *id))
        .collect();
    let mut response = db
        .query(format!(
            "SELECT VALUE count() FROM {TABLE} WHERE project_id IN $projects AND {LIVE} GROUP ALL"
        ))
        .bind(("projects", keys))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    // `GROUP ALL` yields no row at all when nothing matched, which is a
    // count of zero rather than an error.
    let counted: Option<i64> = response.take(0)?;
    Ok(usize::try_from(counted.unwrap_or(0)).unwrap_or(0))
}

/// `(project_id, kind)` for every asset filed across `project_ids`, in one
/// round trip — what the matter-lifecycle fold needs and nothing else, so it
/// reads a narrow projection rather than every [`Asset`] field. An empty
/// slice never reaches the engine.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn kinds_by_projects(
    db: &SurrealDb,
    project_ids: &[Uuid],
) -> Result<Vec<(Uuid, Option<String>)>, AssetError> {
    #[derive(SurrealValue)]
    struct KindRow {
        project_id: Option<surrealdb::types::RecordId>,
        kind: Option<String>,
    }

    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<surrealdb::types::RecordId> = project_ids
        .iter()
        .map(|id| record_id(crate::projects::PROJECT_TABLE, *id))
        .collect();
    let mut response = db
        .query(format!(
            "SELECT project_id, kind FROM {TABLE} WHERE project_id IN $projects AND {LIVE}"
        ))
        .bind(("projects", keys))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<KindRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((record_uuid(r.project_id.as_ref()?)?, r.kind)))
        .collect())
}

/// Every asset, newest first — the `/app/lawyer/assets` transparency listing.
///
/// Unbounded. That is a real
/// cost on a large deployment and the reason this is not a hot path: it is
/// a lawyer directory, not something a matter surface reads.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn list_all(db: &SurrealDb) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!("SELECT {SELECT} FROM {TABLE} ORDER BY id DESC"))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Delete the asset `id`, returning the row that was removed so a caller
/// can decide what to do with its storage object. `None` when no such row
/// existed.
///
/// Deliberately does **not** touch storage: `storage_key` is not unique,
/// so whether the bytes may go is a question about the *other* rows, which
/// only the caller's own bookkeeping can answer. See
/// [`storage_key_is_referenced`].
///
/// # Errors
/// [`AssetError::Db`] if the delete fails.
pub async fn delete(db: &SurrealDb, id: Uuid) -> Result<Option<Asset>, AssetError> {
    let response = writing(|| {
        db.query("DELETE $id RETURN BEFORE")
            .bind(("id", record_id(TABLE, id)))
    })
    .await?;
    let deleted = one(response)?;
    mirror_writes(db, &[id]).await;
    Ok(deleted)
}

/// Whether any asset row still points at `storage_key`.
///
/// The guard before deleting a content-addressed object: identical bytes
/// are stored once and may be referenced by several rows, so the object
/// outlives any one of them.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn storage_key_is_referenced(
    db: &SurrealDb,
    storage_key: &str,
) -> Result<bool, AssetError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} WHERE storage_key = $key LIMIT 1"
        ))
        .bind(("key", storage_key.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let found: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(found.is_some())
}

/// Ingest `bytes` as a bare content asset: dedup by SHA-256, write to
/// storage at `blobs/<sha>` when new, insert/reuse a **bare** `asset` row
/// (no document metadata), and return its id. Idempotent — re-ingesting
/// identical bare content reuses the existing bare asset, which is what
/// keeps a re-read of an unchanged template body from churning a new row.
///
/// The dedup lookup only ever reuses a *bare* asset (`filename` unset), so
/// this lane never hands back a document asset that merely shares bytes.
///
/// # Errors
/// [`AssetError`] on a storage or database failure.
pub async fn ingest_content(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    bytes: &[u8],
    content_type: &str,
) -> Result<Uuid, AssetError> {
    let sha_hex = sha256_hex(bytes);
    if let Some(existing) = find_bare_by_sha(db, &sha_hex).await? {
        return Ok(existing.id);
    }
    let storage_key = format!("blobs/{sha_hex}");
    storage.put(&storage_key, bytes, content_type).await?;
    let byte_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);

    let id = Uuid::now_v7();
    let mut response = writing(|| {
        db.query(format!(
            "CREATE $id SET \
             storage_key = $storage_key, \
             content_type = $content_type, \
             byte_size = $byte_size, \
             sha256_hex = $sha256_hex, \
             visibility = $visibility, \
             metadata = NONE \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("storage_key", storage_key.clone()))
        .bind(("content_type", content_type.to_string()))
        .bind(("byte_size", byte_size))
        .bind(("sha256_hex", sha_hex.clone()))
        .bind((
            "visibility",
            crate::documents::visibility::INTERNAL.to_string(),
        ))
    })
    .await?;

    let row: Option<AssetRow> = response.take(0)?;
    let id = row
        .and_then(AssetRow::into_asset)
        .map(|a| a.id)
        .ok_or(AssetError::WriteReturnedNothing)?;
    mirror_writes(db, &[id]).await;
    Ok(id)
}

/// The bare asset holding `sha_hex`, if one exists. Bare means
/// `filename` unset — a document asset sharing the bytes is a different
/// row with different metadata and must never be handed to this lane.
async fn find_bare_by_sha(db: &SurrealDb, sha_hex: &str) -> Result<Option<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE sha256_hex = $sha AND filename IS NONE LIMIT 1"
        ))
        .bind(("sha", sha_hex.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Fetch an asset's bytes from storage by asset id.
///
/// # Errors
/// [`AssetError`] when the row is missing or storage fails.
pub async fn fetch(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    asset_id: Uuid,
) -> Result<Vec<u8>, AssetError> {
    let row = find_by_id(db, asset_id)
        .await?
        .ok_or_else(|| AssetError::Storage(cloud::StorageError::NotFound(asset_id.to_string())))?;
    Ok(storage.get(&row.storage_key).await?.bytes)
}

/// Which revision of a slugged document a given reader is looking at.
///
/// Not a role and not an ACL — the project ACL has already decided the
/// reader may see the matter at all. This picks *which revision* of a
/// document that reader's view is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    /// Lawyers see the latest revision, published or not — the redline they
    /// are still working on.
    Lawyer,
    /// A client sees the latest revision that is both published and
    /// marked client-visible.
    Client,
}

/// The **operative version** of the document `(project_id, slug)` under
/// `lens`, or `None` when the chain is empty for that reader.
///
/// One rule gives both behaviours the asset lane needs:
///
/// - *Official ≠ latest.* An unpublished redline sitting above the
///   executed agreement changes nothing for the client — the
///   NetDocuments "Official Version" semantics without an `is_current`
///   flag to keep in sync.
/// - *Latest visible to you.* The client keeps seeing v2 while lawyer
///   iterate on v3.
///
/// **Insertion order decides current**, not `published_at`. Ids are
/// UUIDv7, so `id` descending is newest-first insertion order, and a
/// back-dated `published_at` (a court's file stamp) therefore cannot
/// reorder a chain — it stays display and sort metadata. That ordering
/// survived the port unchanged: it is a property of how the ids are
/// minted, not of the engine underneath. History remains reachable
/// through [`revisions`].
///
/// Expressed here once so no call site reimplements it and drifts.
///
/// # Errors
/// Propagates any database error.
pub async fn current(
    db: &SurrealDb,
    project_id: Uuid,
    slug: &str,
    lens: Lens,
) -> Result<Option<Asset>, AssetError> {
    let lens_filter = match lens {
        Lens::Lawyer => "",
        Lens::Client => " AND published_at IS NOT NONE AND visibility = $visible",
    };
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project AND slug = $slug AND {LIVE}{lens_filter} \
             ORDER BY id DESC LIMIT 1"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("slug", slug.to_string()))
        .bind(("visible", crate::documents::visibility::CLIENT.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Every revision of `(project_id, slug)`, newest first.
///
/// The "dig deep and see earlier versions" path. Unfiltered by lens on
/// purpose — callers that show history to a client filter it themselves,
/// because "which revisions existed" is a different question from "which
/// one is operative".
///
/// # Errors
/// Propagates any database error.
pub async fn revisions(
    db: &SurrealDb,
    project_id: Uuid,
    slug: &str,
) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project AND slug = $slug ORDER BY id DESC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("slug", slug.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Every revision of the requested document identities on one Project,
/// newest first within each slug. Soft-deleted rows stay in the result so
/// callers can apply the same visibility and publication lens to full history.
///
/// One Project-and-slug-scoped query serves the complete page regardless of
/// how many document identities it contains.
///
/// # Errors
/// Propagates any database error.
pub async fn revisions_for_slugs(
    db: &SurrealDb,
    project_id: Uuid,
    slugs: &[String],
) -> Result<Vec<Asset>, AssetError> {
    if slugs.is_empty() {
        return Ok(Vec::new());
    }
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE project_id = $project AND slug IN $slugs ORDER BY slug ASC, id DESC"
        ))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .bind(("slugs", slugs.to_vec()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Set the visibility of one asset that belongs to `project_id`.
///
/// Returns `None` when the asset does not belong to that Project, otherwise
/// whether the stored value changed. The Project scope is part of the write so
/// an asset id is never an authorization bypass.
///
/// # Errors
/// Propagates any database error.
pub async fn set_visibility(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    visibility: &str,
) -> Result<Option<bool>, AssetError> {
    let current = find_by_id(db, asset_id).await?;
    let Some(asset) = current.filter(|asset| asset.project_id == Some(project_id)) else {
        return Ok(None);
    };
    if asset.visibility == visibility {
        return Ok(Some(false));
    }
    let response = writing(|| {
        db.query("UPDATE $id SET visibility = $visibility, updated_at = time::now()")
            .bind(("id", record_id(TABLE, asset_id)))
            .bind(("visibility", visibility.to_string()))
    })
    .await?;
    if one(response)?.is_none() {
        return Err(AssetError::WriteReturnedNothing);
    }
    mirror_writes(db, &[asset_id]).await;
    Ok(Some(true))
}

/// Days a soft-deleted document stays recoverable before the purge may
/// expunge it.
pub const RETENTION_DAYS: i64 = 180;

/// What a soft deletion stamped on one document identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SoftDeletion {
    pub slug: Option<String>,
    /// How many revisions the chain holds; all carry the stamp.
    pub revisions: usize,
    pub deleted_at: DateTime<Utc>,
    pub purge_after: DateTime<Utc>,
    /// `false` when the chain was already soft-deleted: the original stamps
    /// stand and nothing was written.
    pub changed: bool,
}

/// Why [`soft_delete`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum SoftDeleteError {
    /// The asset is missing or belongs to another Project.
    #[error("asset not found on this project")]
    NotOnProject,
    /// A `closed_repository` archive is the record that makes closing a
    /// matter safe; it is never deleted.
    #[error("a closed_repository document is never soft-deleted")]
    ClosedRepository,
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// Every revision of the document identity `asset` belongs to — its slug
/// chain, or the row alone when it has no slug — soft-deleted or not.
async fn chain_of(db: &SurrealDb, asset: &Asset) -> Result<Vec<Asset>, AssetError> {
    let (Some(project_id), Some(slug)) = (asset.project_id, asset.slug.as_deref()) else {
        return Ok(vec![asset.clone()]);
    };
    let mut chain = revisions(db, project_id, slug).await?;
    if !chain.iter().any(|row| row.id == asset.id) {
        chain.push(asset.clone());
    }
    Ok(chain)
}

/// Soft-delete the document identity `asset_id` belongs to: every revision
/// of its chain is stamped with `deleted_at = now`, `purge_after = now +`
/// [`RETENTION_DAYS`], the person who deleted it, and, when a repository
/// pointer removal caused it, that source commit. Direct retirement has no
/// repository commit. The revision chain is kept; [`restore`] undoes it.
///
/// Deleting a chain that is already deleted changes nothing and reports the
/// original stamps, so a sync that stopped after the write can run again.
///
/// # Errors
/// [`SoftDeleteError`] for a row off the Project, a `closed_repository`
/// document, or a database failure.
pub async fn soft_delete(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    deleted_by: Uuid,
    commit_sha: Option<&str>,
    now: DateTime<Utc>,
) -> Result<SoftDeletion, SoftDeleteError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(SoftDeleteError::NotOnProject);
    };
    if asset.kind.as_deref() == Some(rules::kind::Kind::ClosedRepository.as_str()) {
        return Err(SoftDeleteError::ClosedRepository);
    }
    let chain = chain_of(db, &asset).await?;
    if let Some((deleted_at, purge_after)) = chain
        .iter()
        .all(|row| row.deleted_at.is_some())
        .then(|| {
            chain
                .iter()
                .find_map(|row| row.deleted_at.zip(row.purge_after))
        })
        .flatten()
    {
        return Ok(SoftDeletion {
            slug: asset.slug,
            revisions: chain.len(),
            deleted_at,
            purge_after,
            changed: false,
        });
    }
    let purge_after = now + chrono::Duration::days(RETENTION_DAYS);
    for row in chain.iter().filter(|row| row.deleted_at.is_none()) {
        let response = writing(|| {
            db.query(
                "UPDATE $id SET deleted_at = $deleted_at, purge_after = $purge_after, \
                 deleted_by_person_id = $person, deleted_commit_sha = $sha, \
                 updated_at = time::now()",
            )
            .bind(("id", record_id(TABLE, row.id)))
            .bind(("deleted_at", surrealdb::types::Datetime::from(now)))
            .bind(("purge_after", surrealdb::types::Datetime::from(purge_after)))
            .bind(("person", record_id("person", deleted_by)))
            .bind(("sha", commit_sha.map(str::to_string)))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
    }
    let stamped: Vec<Uuid> = chain
        .iter()
        .filter(|row| row.deleted_at.is_none())
        .map(|row| row.id)
        .collect();
    mirror_writes(db, &stamped).await;
    Ok(SoftDeletion {
        slug: asset.slug,
        revisions: chain.len(),
        deleted_at: now,
        purge_after,
        changed: true,
    })
}

/// What [`restore`] brought back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Restoration {
    pub slug: Option<String>,
    pub revisions: usize,
    /// `false` when the chain was not soft-deleted.
    pub changed: bool,
}

/// Why [`restore`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("asset not found on this project")]
    NotOnProject,
    /// The purge already expunged the bytes; there is nothing to restore.
    #[error("the document was purged after its retention window")]
    Purged,
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// Clear the soft-deletion stamps on every revision of the chain
/// `asset_id` belongs to, returning the document and its full revision
/// history to every default read.
///
/// # Errors
/// [`RestoreError`] for a row off the Project, a purged chain, or a database
/// failure.
pub async fn restore(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
) -> Result<Restoration, RestoreError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(RestoreError::NotOnProject);
    };
    let chain = chain_of(db, &asset).await?;
    if chain.iter().any(|row| row.purged_at.is_some()) {
        return Err(RestoreError::Purged);
    }
    let mut changed = false;
    for row in chain.iter().filter(|row| row.deleted_at.is_some()) {
        let response = writing(|| {
            db.query(
                "UPDATE $id SET deleted_at = NONE, purge_after = NONE, \
                 deleted_by_person_id = NONE, deleted_commit_sha = NONE, \
                 updated_at = time::now()",
            )
            .bind(("id", record_id(TABLE, row.id)))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
        changed = true;
    }
    let cleared: Vec<Uuid> = chain
        .iter()
        .filter(|row| row.deleted_at.is_some())
        .map(|row| row.id)
        .collect();
    mirror_writes(db, &cleared).await;
    Ok(Restoration {
        slug: asset.slug,
        revisions: chain.len(),
        changed,
    })
}

/// Place or release a Legal Hold on the document identity `asset_id` belongs
/// to. A held chain is never purged. Returns `None` when the asset is not on
/// the Project, otherwise how many revisions now carry the stamp.
///
/// # Errors
/// [`AssetError`] on a database failure.
pub async fn set_legal_hold(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    held: bool,
    now: DateTime<Utc>,
) -> Result<Option<usize>, AssetError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Ok(None);
    };
    let chain = chain_of(db, &asset).await?;
    for row in &chain {
        let response = writing(|| {
            let query = if held {
                "UPDATE $id SET legal_hold_at = $now, updated_at = time::now()"
            } else {
                "UPDATE $id SET legal_hold_at = NONE, updated_at = time::now()"
            };
            db.query(query)
                .bind(("id", record_id(TABLE, row.id)))
                .bind(("now", surrealdb::types::Datetime::from(now)))
        })
        .await?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing);
        }
    }
    let held: Vec<Uuid> = chain.iter().map(|row| row.id).collect();
    mirror_writes(db, &held).await;
    Ok(Some(chain.len()))
}

/// Every soft-deleted row whose retention window has closed and whose bytes
/// the purge has not yet expunged, oldest first. The caller still decides
/// which of them a Legal Hold or an unarchived matter protects.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn due_for_purge(db: &SurrealDb, now: DateTime<Utc>) -> Result<Vec<Asset>, AssetError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE deleted_at IS NOT NONE \
             AND purged_at IS NONE AND purge_after <= $now ORDER BY id ASC"
        ))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}

/// Record that the governed expunge removed the bytes of every row in `ids`.
///
/// # Errors
/// [`AssetError`] on a database failure.
pub async fn mark_purged(
    db: &SurrealDb,
    ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<(), AssetError> {
    for id in ids {
        let response = writing(|| {
            db.query("UPDATE $id SET purged_at = $now, updated_at = time::now()")
                .bind(("id", record_id(TABLE, *id)))
                .bind(("now", surrealdb::types::Datetime::from(now)))
        })
        .await?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing);
        }
    }
    mirror_writes(db, ids).await;
    Ok(())
}

/// What restoring one archived row did to the target store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RowRestore {
    /// The row was absent and now exists with its archived identity.
    Created,
    /// An identical row already existed; nothing was written.
    Identical,
    /// A row with the same identity but different content exists; nothing
    /// was written, and the caller must not treat the restore as complete.
    Conflict,
}

/// Write one archived asset row back under its original id, every field as
/// archived: revision chain, checksum, storage key, visibility, provenance,
/// Soft Deletion stamps, and Legal Hold. An identical existing row is left
/// alone and a different one is reported as [`RowRestore::Conflict`], so a
/// restore never widens access or overwrites a live row.
///
/// # Errors
/// [`AssetError`] on a database failure.
pub async fn restore_archived(db: &SurrealDb, asset: &Asset) -> Result<RowRestore, AssetError> {
    if let Some(existing) = find_by_id(db, asset.id).await? {
        return Ok(if &existing == asset {
            RowRestore::Identical
        } else {
            RowRestore::Conflict
        });
    }
    let datetime = |value: Option<DateTime<Utc>>| value.map(surrealdb::types::Datetime::from);
    let response = writing(|| {
        db.query(format!(
            "CREATE $id SET storage_key = $storage_key, \
             secondary_storage_key = $secondary_storage_key, content_type = $content_type, \
             byte_size = $byte_size, sha256_hex = $sha256_hex, project_id = $project_id, \
             filename = $filename, kind = $kind, source = $source, received_at = $received_at, \
             description = $description, visibility = $visibility, slug = $slug, \
             published_at = $published_at, source_message_id = $source_message_id, \
             source_sender = $source_sender, source_received_at = $source_received_at, \
             source_subject = $source_subject, metadata = $metadata, \
             derived_from = $derived_from, transcript_quality = $transcript_quality, \
             signing = $signing, notarization = $notarization, deleted_at = $deleted_at, \
             purge_after = $purge_after, deleted_by_person_id = $deleted_by, \
             deleted_commit_sha = $deleted_commit_sha, legal_hold_at = $legal_hold_at, \
             purged_at = $purged_at, inserted_at = $inserted_at, updated_at = $updated_at \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, asset.id)))
        .bind(("storage_key", asset.storage_key.clone()))
        .bind(("secondary_storage_key", asset.secondary_storage_key.clone()))
        .bind(("content_type", asset.content_type.clone()))
        .bind(("byte_size", asset.byte_size))
        .bind(("sha256_hex", asset.sha256_hex.clone()))
        .bind((
            "project_id",
            asset
                .project_id
                .map(|id| record_id(crate::projects::PROJECT_TABLE, id)),
        ))
        .bind(("filename", asset.filename.clone()))
        .bind(("kind", asset.kind.clone()))
        .bind(("source", asset.source.clone()))
        .bind(("received_at", asset.received_at.clone()))
        .bind(("description", asset.description.clone()))
        .bind(("visibility", asset.visibility.clone()))
        .bind(("slug", asset.slug.clone()))
        .bind(("published_at", asset.published_at.clone()))
        .bind(("source_message_id", asset.source_message_id.clone()))
        .bind(("source_sender", asset.source_sender.clone()))
        .bind(("source_received_at", asset.source_received_at.clone()))
        .bind(("source_subject", asset.source_subject.clone()))
        .bind(("metadata", asset.metadata.clone()))
        .bind(("derived_from", asset.derived_from.clone()))
        .bind(("transcript_quality", asset.transcript_quality.clone()))
        .bind((
            "signing",
            asset
                .signing
                .and_then(|signing| serde_json::to_value(signing).ok()),
        ))
        .bind((
            "notarization",
            (!asset.notarization.is_empty())
                .then(|| serde_json::to_value(&asset.notarization).ok())
                .flatten(),
        ))
        .bind(("deleted_at", datetime(asset.deleted_at)))
        .bind(("purge_after", datetime(asset.purge_after)))
        .bind((
            "deleted_by",
            asset.deleted_by_person_id.map(|id| record_id("person", id)),
        ))
        .bind(("deleted_commit_sha", asset.deleted_commit_sha.clone()))
        .bind(("legal_hold_at", datetime(asset.legal_hold_at)))
        .bind(("purged_at", datetime(asset.purged_at)))
        .bind((
            "inserted_at",
            surrealdb::types::Datetime::from(asset.inserted_at),
        ))
        .bind((
            "updated_at",
            surrealdb::types::Datetime::from(asset.updated_at),
        ))
    })
    .await?;
    if one(response)?.is_none() {
        return Err(AssetError::WriteReturnedNothing);
    }
    mirror_writes(db, &[asset.id]).await;
    Ok(RowRestore::Created)
}

/// Why [`assign_slug`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum AssignSlugError {
    /// The asset is missing or belongs to another Project. Callers map this
    /// to the same not-found response as an out-of-scope matter.
    #[error("asset is not on this project")]
    NotOnProject,
    /// A slug is the chain identity. A row that already has one is not renamed
    /// here, and two chains are never merged.
    #[error("the row already has a slug")]
    AlreadySlugged,
    /// `slug` already names at least one revision on this Project.
    #[error("slug `{0}` already names a document on this project")]
    ChainExists(String),
    /// Slug validation needs the filename stored on the row.
    #[error("the row has no filename")]
    MissingFilename,
    #[error(transparent)]
    InvalidSlug(#[from] crate::documents::DocumentSlugError),
    /// `kind` is not a [`rules::kind::Kind`] valid for [`rules::kind::Lane::Asset`].
    #[error("`{0}` is not a document kind that can be filed on a matter")]
    InvalidKind(String),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// The slug, and optional kind, to set on a row that has neither a slug nor
/// a revision chain.
pub struct AssignSlug<'a> {
    pub slug: &'a str,
    pub kind: Option<&'a str>,
    /// Validate and report the row that would be written. The stored row is
    /// left unchanged.
    pub dry_run: bool,
}

/// Set `slug` on one asset that belongs to `project_id` and whose slug is
/// null. `assignment.kind`, when set, replaces `kind` in the same write.
/// `sha256_hex`, `storage_key`, and `byte_size` stay as stored.
///
/// A row that already has a slug, and a slug that already has any revision
/// on this Project, are both refused before a write. The second refusal is
/// what keeps this from merging two chains.
///
/// # Errors
/// [`AssignSlugError`] when the row is out of the Project, already slugged,
/// the slug collides with a chain, or `slug` / `kind` fail validation.
pub async fn assign_slug(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    assignment: AssignSlug<'_>,
) -> Result<Asset, AssignSlugError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(AssignSlugError::NotOnProject);
    };
    if asset.slug.is_some() {
        return Err(AssignSlugError::AlreadySlugged);
    }
    let filename = asset
        .filename
        .as_deref()
        .filter(|filename| !filename.is_empty())
        .ok_or(AssignSlugError::MissingFilename)?;
    let slug = assignment.slug.trim();
    crate::documents::validate_document_slug(filename, slug)?;
    let kind = match assignment
        .kind
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
    {
        Some(kind) => {
            if !rules::kind::Kind::parse(kind)
                .is_some_and(|parsed| parsed.valid_for(rules::kind::Lane::Asset))
            {
                return Err(AssignSlugError::InvalidKind(kind.to_string()));
            }
            Some(kind.to_string())
        }
        None => None,
    };
    if !revisions(db, project_id, slug).await?.is_empty() {
        return Err(AssignSlugError::ChainExists(slug.to_string()));
    }
    if assignment.dry_run {
        let mut preview = asset;
        preview.slug = Some(slug.to_string());
        if let Some(kind) = kind {
            preview.kind = Some(kind);
        }
        return Ok(preview);
    }
    let response = match &kind {
        Some(kind) => writing(|| {
            db.query("UPDATE $id SET slug = $slug, kind = $kind, updated_at = time::now()")
                .bind(("id", record_id(TABLE, asset_id)))
                .bind(("slug", slug.to_string()))
                .bind(("kind", kind.clone()))
        })
        .await
        .map_err(AssetError::from)?,
        None => writing(|| {
            db.query("UPDATE $id SET slug = $slug, updated_at = time::now()")
                .bind(("id", record_id(TABLE, asset_id)))
                .bind(("slug", slug.to_string()))
        })
        .await
        .map_err(AssetError::from)?,
    };
    if one(response)?.is_none() {
        return Err(AssetError::WriteReturnedNothing.into());
    }
    mirror_writes(db, &[asset_id]).await;
    let written = find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    if written.sha256_hex != asset.sha256_hex
        || written.storage_key != asset.storage_key
        || written.byte_size != asset.byte_size
        || written.id != asset.id
    {
        return Err(AssetError::WriteReturnedNothing.into());
    }
    Ok(written)
}

/// Why [`reclassify_kind`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum ReclassifyKindError {
    /// The asset is missing or belongs to another Project.
    #[error("asset is not on this project")]
    NotOnProject,
    /// The row's kind is already an asset-lane kind. A chain's kind is stable
    /// once it is valid; this door only repairs a kind the lane rejects.
    #[error("kind `{0}` is already an accepted document kind")]
    KindAccepted(String),
    /// Another revision in the row's chain already carries an accepted kind,
    /// so rewriting the chain would change a valid classification.
    #[error("another revision of `{0}` already carries an accepted kind")]
    ChainHasAcceptedKind(String),
    /// `kind` is not a [`rules::kind::Kind`] valid for [`rules::kind::Lane::Asset`].
    #[error("`{0}` is not a document kind that can be filed on a matter")]
    InvalidKind(String),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

fn is_asset_kind(kind: Option<&str>) -> bool {
    kind.and_then(rules::kind::Kind::parse)
        .is_some_and(|parsed| parsed.valid_for(rules::kind::Lane::Asset))
}

/// Replace a `kind` that is not an asset-lane kind — a legacy free-text
/// classification that predates the closed list — on one asset that belongs
/// to `project_id`, and on every other revision of its slug chain.
///
/// A row whose kind is already accepted is refused, so this never changes one
/// valid classification into another. A slugged row is rewritten with its
/// whole chain, because a revision cannot change the chain's kind; a chain
/// where any revision already carries an accepted kind is refused.
/// `sha256_hex`, `storage_key`, `byte_size`, and `slug` stay as stored.
/// Returns the requested row as written (or, with `dry_run`, as it would be).
///
/// # Errors
/// [`ReclassifyKindError`] when the row is out of the Project, its kind (or a
/// chain sibling's) is already accepted, or `kind` fails validation.
pub async fn reclassify_kind(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    kind: &str,
    dry_run: bool,
) -> Result<Asset, ReclassifyKindError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(ReclassifyKindError::NotOnProject);
    };
    if is_asset_kind(asset.kind.as_deref()) {
        return Err(ReclassifyKindError::KindAccepted(
            asset.kind.clone().unwrap_or_default(),
        ));
    }
    let kind = kind.trim();
    if !is_asset_kind(Some(kind)) {
        return Err(ReclassifyKindError::InvalidKind(kind.to_string()));
    }
    let chain = match asset.slug.as_deref() {
        Some(slug) => revisions(db, project_id, slug).await?,
        None => Vec::new(),
    };
    if let Some(slug) = asset.slug.as_deref() {
        if chain.iter().any(|row| is_asset_kind(row.kind.as_deref())) {
            return Err(ReclassifyKindError::ChainHasAcceptedKind(slug.to_string()));
        }
    }
    if dry_run {
        let mut preview = asset;
        preview.kind = Some(kind.to_string());
        return Ok(preview);
    }
    let mut ids: Vec<Uuid> = chain.iter().map(|row| row.id).collect();
    if !ids.contains(&asset.id) {
        ids.push(asset.id);
    }
    for id in ids.iter().copied() {
        let response = writing(|| {
            db.query("UPDATE $id SET kind = $kind, updated_at = time::now()")
                .bind(("id", record_id(TABLE, id)))
                .bind(("kind", kind.to_string()))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
    }
    mirror_writes(db, &ids).await;
    let written = find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    if written.sha256_hex != asset.sha256_hex
        || written.storage_key != asset.storage_key
        || written.byte_size != asset.byte_size
        || written.slug != asset.slug
    {
        return Err(AssetError::WriteReturnedNothing.into());
    }
    Ok(written)
}

/// Why [`reclassify_accepted_kind`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum ReclassifyAcceptedKindError {
    /// The asset is missing or belongs to another Project.
    #[error("asset is not on this project")]
    NotOnProject,
    /// The row's kind is not an asset-lane kind; [`reclassify_kind`] is the
    /// door that replaces a legacy kind.
    #[error("kind `{0}` is not an accepted document kind; replace it with `site document kind`")]
    KindNotAccepted(String),
    /// The target is the kind the row already carries.
    #[error("the document is already filed as `{0}`")]
    KindUnchanged(String),
    /// `kind` is not a [`rules::kind::Kind`] valid for [`rules::kind::Lane::Asset`].
    #[error("`{0}` is not a document kind that can be filed on a matter")]
    InvalidKind(String),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// Change a document from one accepted asset-lane kind to another on one
/// asset that belongs to `project_id`, and on every other revision of its
/// slug chain, because a revision cannot change its chain's kind.
///
/// This is the correction for a document filed under the wrong valid kind.
/// A row whose kind is not accepted is refused and pointed at
/// [`reclassify_kind`]; a target equal to the current kind is refused.
/// `sha256_hex`, `storage_key`, `byte_size`, and `slug` stay as stored.
/// Returns the requested row as written (or, with `dry_run`, as it would be).
///
/// # Errors
/// [`ReclassifyAcceptedKindError`] when the row is out of the Project, its
/// kind is not accepted, `kind` fails validation, or nothing would change.
pub async fn reclassify_accepted_kind(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    kind: &str,
    dry_run: bool,
) -> Result<Asset, ReclassifyAcceptedKindError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(ReclassifyAcceptedKindError::NotOnProject);
    };
    let current = asset.kind.clone().unwrap_or_default();
    if !is_asset_kind(Some(&current)) {
        return Err(ReclassifyAcceptedKindError::KindNotAccepted(current));
    }
    let kind = kind.trim();
    if !is_asset_kind(Some(kind)) {
        return Err(ReclassifyAcceptedKindError::InvalidKind(kind.to_string()));
    }
    if kind == current {
        return Err(ReclassifyAcceptedKindError::KindUnchanged(current));
    }
    if dry_run {
        let mut preview = asset;
        preview.kind = Some(kind.to_string());
        return Ok(preview);
    }
    let mut ids: Vec<Uuid> = match asset.slug.as_deref() {
        Some(slug) => revisions(db, project_id, slug)
            .await?
            .iter()
            .map(|row| row.id)
            .collect(),
        None => Vec::new(),
    };
    if !ids.contains(&asset.id) {
        ids.push(asset.id);
    }
    for id in ids.iter().copied() {
        let response = writing(|| {
            db.query("UPDATE $id SET kind = $kind, updated_at = time::now()")
                .bind(("id", record_id(TABLE, id)))
                .bind(("kind", kind.to_string()))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
    }
    mirror_writes(db, &ids).await;
    let written = find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    if written.sha256_hex != asset.sha256_hex
        || written.storage_key != asset.storage_key
        || written.byte_size != asset.byte_size
        || written.slug != asset.slug
    {
        return Err(AssetError::WriteReturnedNothing.into());
    }
    Ok(written)
}

/// Why [`reslug`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum ReslugError {
    /// The asset is missing or belongs to another Project.
    #[error("asset is not on this project")]
    NotOnProject,
    /// The row has no slug yet; [`assign_slug`] is the door for that case.
    #[error("row has no slug to move")]
    NoSlug,
    /// The row has no filename to check either slug against.
    #[error("the document has no filename to check the slug against")]
    MissingFilename,
    /// The target equals the current slug, so nothing would move.
    #[error("slug `{0}` is already this document's identity")]
    SlugUnchanged(String),
    /// The target is not itself a valid identity for this row's filename.
    #[error(transparent)]
    InvalidTarget(#[from] crate::documents::DocumentSlugError),
    /// Another chain on this Project already holds the target slug.
    #[error("slug `{0}` already names a document on this matter")]
    TargetOccupied(String),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// Move a document identity — a legacy slug stored with no extension of its
/// own (LAW-26, LAW-62), or a complete valid one such as `notes/a.pdf` moving
/// to `filed/forms/a.pdf` — to another slug that is valid for the row's
/// filename.
///
/// Refuses a target equal to the current slug, and a target that already
/// names a live chain on this Project (an occupied identity is never
/// overwritten or merged). Every revision of the row's chain moves together,
/// so the chain's history stays keyed to one slug throughout. `sha256_hex`,
/// `storage_key`, `byte_size`, and `kind` stay as stored. Returns the
/// requested row as written (or, with `dry_run`, as it would be).
///
/// # Errors
/// [`ReslugError`] when the row is out of the Project, has no slug, the
/// target is unchanged or not a valid identity for this row's filename, or
/// the target already names a chain.
pub async fn reslug(
    db: &SurrealDb,
    project_id: Uuid,
    asset_id: Uuid,
    new_slug: &str,
    dry_run: bool,
) -> Result<Asset, ReslugError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(ReslugError::NotOnProject);
    };
    let current_slug = asset
        .slug
        .clone()
        .filter(|slug| !slug.is_empty())
        .ok_or(ReslugError::NoSlug)?;
    let filename = asset
        .filename
        .as_deref()
        .filter(|filename| !filename.is_empty())
        .ok_or(ReslugError::MissingFilename)?;
    let new_slug = new_slug.trim();
    crate::documents::validate_document_slug(filename, new_slug)?;
    if new_slug == current_slug {
        return Err(ReslugError::SlugUnchanged(current_slug));
    }
    if !revisions(db, project_id, new_slug).await?.is_empty() {
        return Err(ReslugError::TargetOccupied(new_slug.to_string()));
    }
    let chain = revisions(db, project_id, &current_slug).await?;
    if dry_run {
        let mut preview = asset;
        preview.slug = Some(new_slug.to_string());
        return Ok(preview);
    }
    let mut ids: Vec<Uuid> = chain.iter().map(|row| row.id).collect();
    if !ids.contains(&asset.id) {
        ids.push(asset.id);
    }
    for id in ids.iter().copied() {
        let response = writing(|| {
            db.query("UPDATE $id SET slug = $slug, updated_at = time::now()")
                .bind(("id", record_id(TABLE, id)))
                .bind(("slug", new_slug.to_string()))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
    }
    mirror_writes(db, &ids).await;
    let written = find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    if written.sha256_hex != asset.sha256_hex
        || written.storage_key != asset.storage_key
        || written.byte_size != asset.byte_size
        || written.kind != asset.kind
    {
        return Err(AssetError::WriteReturnedNothing.into());
    }
    Ok(written)
}

/// Why [`repair_missing_object`] changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum RepairStorageError {
    #[error("asset is not on this project")]
    NotOnProject,
    /// The object is present and its digest does not match the row. This
    /// repair only restores a missing object.
    #[error("the stored object does not match the recorded sha256")]
    ObjectCorrupt,
    /// No other asset on this Project has the same sha256 and a readable object.
    #[error("no sibling in this project holds the content")]
    NoSibling,
    /// The bytes copied from the sibling do not hash to the row's sha256.
    #[error("repaired bytes do not match the recorded sha256")]
    DigestMismatch,
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
}

/// What [`repair_missing_object`] did, or would do when `dry_run` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairedObject {
    pub storage_key: String,
    /// Bytes were copied onto the content-addressed key. False when the
    /// repair only re-points the row at a key that already holds them.
    pub copied: bool,
    pub dry_run: bool,
}

/// Restore a missing storage object from another asset on the same Project
/// with the same `sha256_hex`.
///
/// The restored key is the content-addressed documents key
/// (`projects/<code>/documents/<sha256>`). When the row already names that
/// key, the sibling's bytes are copied onto it. When it names a different
/// key, the row is re-pointed at the content-addressed key after that object
/// exists. `sha256_hex` and `byte_size` stay as stored. A Project with no
/// sibling whose object exists is refused and left unchanged.
///
/// # Errors
/// [`RepairStorageError`] when the asset is out of the Project, the present
/// object does not match the recorded digest, or no sibling holds the bytes.
pub async fn repair_missing_object(
    db: &SurrealDb,
    storage: &dyn StorageService,
    project_id: Uuid,
    asset_id: Uuid,
    dry_run: bool,
) -> Result<RepairedObject, RepairStorageError> {
    let Some(asset) = find_by_id(db, asset_id)
        .await?
        .filter(|asset| asset.project_id == Some(project_id))
    else {
        return Err(RepairStorageError::NotOnProject);
    };
    let project = crate::projects::find_by_id(db, project_id).await?.ok_or(
        crate::projects::ProjectStoreError::NoSuchProject(project_id),
    )?;
    let canonical = format!(
        "{}/{}",
        cloud::workspace::documents_prefix(&project.code),
        asset.sha256_hex
    );
    if object_head(storage, &asset.storage_key).await?.is_some() {
        let digest = object_sha256(storage, &asset.storage_key).await?;
        if digest.as_deref() != Some(asset.sha256_hex.as_str()) {
            return Err(RepairStorageError::ObjectCorrupt);
        }
        return Ok(RepairedObject {
            storage_key: asset.storage_key,
            copied: false,
            dry_run,
        });
    }

    let siblings = for_project(db, project_id).await?;
    let mut source: Option<Vec<u8>> = None;
    for sibling in siblings
        .into_iter()
        .filter(|sibling| sibling.id != asset.id && sibling.sha256_hex == asset.sha256_hex)
    {
        let keys = [Some(sibling.storage_key), sibling.secondary_storage_key];
        for key in keys.into_iter().flatten() {
            if object_head(storage, &key).await?.is_none() {
                continue;
            }
            let object = storage.get(&key).await.map_err(AssetError::from)?;
            if sha256_hex(&object.bytes) == asset.sha256_hex {
                source = Some(object.bytes);
                break;
            }
        }
        if source.is_some() {
            break;
        }
    }
    let Some(bytes) = source else {
        return Err(RepairStorageError::NoSibling);
    };
    if sha256_hex(&bytes) != asset.sha256_hex {
        return Err(RepairStorageError::DigestMismatch);
    }

    let copied = object_head(storage, &canonical).await?.is_none();
    let repoint = asset.storage_key != canonical;
    if dry_run {
        return Ok(RepairedObject {
            storage_key: canonical,
            copied,
            dry_run: true,
        });
    }
    if copied {
        storage
            .put(&canonical, &bytes, &asset.content_type)
            .await
            .map_err(AssetError::from)?;
    }
    if repoint {
        let response = writing(|| {
            db.query("UPDATE $id SET storage_key = $storage_key, updated_at = time::now()")
                .bind(("id", record_id(TABLE, asset_id)))
                .bind(("storage_key", canonical.clone()))
        })
        .await
        .map_err(AssetError::from)?;
        if one(response)?.is_none() {
            return Err(AssetError::WriteReturnedNothing.into());
        }
        mirror_writes(db, &[asset_id]).await;
    }
    let verified = object_sha256(storage, &canonical).await?;
    if verified.as_deref() != Some(asset.sha256_hex.as_str()) {
        return Err(RepairStorageError::DigestMismatch);
    }
    Ok(RepairedObject {
        storage_key: canonical,
        copied,
        dry_run: false,
    })
}

/// Why a [`file_revision`] call wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum RevisionError {
    /// The kind names a Markdown lane value (`workshop`, `post`) or is
    /// not vocabulary at all. The asset lane is closed: see
    /// [`rules::kind::Kind::valid_for`].
    #[error("`{0}` is not a document kind that can be filed on a matter")]
    KindNotFilable(String),
    /// The chain already exists under a different kind. A changed kind is
    /// a *different document*, not a new revision of this one.
    #[error("document `{slug}` is a `{existing}`; a `{attempted}` is a different document")]
    KindChanged {
        slug: String,
        existing: String,
        attempted: String,
    },
    #[error(transparent)]
    Ingest(#[from] crate::documents::IngestError),
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// What [`file_revision`] did.
#[derive(Debug, Clone)]
pub enum Filed {
    /// A new revision was appended; the chain grew by one row.
    Revision(crate::documents::IngestedDocument),
    /// The bytes were identical to the chain's newest revision, so
    /// nothing was written. Carries that row's id so a caller can link to
    /// the document it already has.
    Unchanged { asset_id: Uuid },
}

/// File `bytes` as the next revision of the document `(project_id,
/// identity.slug)` — the **asset lane's write boundary**.
///
/// Three rules live here and nowhere else, so no call site reimplements
/// one and drifts:
///
/// 1. **The lane is closed.** `args.kind` must be
///    [`rules::kind::Lane::Asset`]-valid. A teaching page or a dashboard
///    skeleton is never a byte artifact filed on a matter.
/// 2. **Kind is immutable across a chain.** A slug that already holds a
///    `retainer` does not accept an `agreement` revision — that is a
///    different document and belongs under its own slug.
/// 3. **Identical bytes are a no-op**, scoped to this one slug on this
///    one project. Never a global "we already have these bytes" probe:
///    that is an existence oracle, and legal documents are boilerplate
///    plus a few low-entropy fields — exactly the shape Harnik et al.
///    (IEEE S&P 2010) showed lets an outsider confirm a document's
///    contents by uploading a guess. The same bytes under a *different*
///    slug are a genuinely different document and do create a revision.
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, or on a storage or
/// database failure.
pub async fn file_revision(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(db, storage, args, identity, None, bytes).await
}

/// [`file_revision`], additionally stamping [`crate::documents::MailProvenance`]
/// onto a newly-inserted revision (ENG-517). The one caller is `navigator
/// site mail file`'s server-side handler.
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, or on a storage or
/// database failure.
pub async fn file_revision_with_provenance(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: &crate::documents::MailProvenance<'_>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(db, storage, args, identity, Some(provenance), bytes).await
}

async fn file_revision_inner(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    // Rule 1 — the lane is closed. An unrecognized string is rejected by
    // the same arm as a valid-but-wrong-lane one: neither classifies a
    // document that can be filed on a matter.
    if !rules::kind::Kind::parse(args.kind).is_some_and(|k| k.valid_for(rules::kind::Lane::Asset)) {
        return Err(RevisionError::KindNotFilable(args.kind.to_string()));
    }

    // An unslugged artifact is a one-off: it is a revision of nothing, so
    // rules 2 and 3 have no chain to consult.
    let Some(slug) = identity.slug else {
        let doc =
            ingest_with_optional_provenance(db, storage, args, identity, provenance, bytes).await?;
        return Ok(Filed::Revision(doc));
    };

    let latest = current(db, args.project_id, slug, Lens::Lawyer).await?;

    if let Some(head) = latest {
        // Rule 2 — kind is immutable across the chain.
        if let Some(existing) = head.kind.as_deref() {
            if existing != args.kind {
                return Err(RevisionError::KindChanged {
                    slug: slug.to_string(),
                    existing: existing.to_string(),
                    attempted: args.kind.to_string(),
                });
            }
        }
        // Rule 3 — identical bytes change nothing.
        if head.sha256_hex == sha256_hex(bytes) {
            return Ok(Filed::Unchanged { asset_id: head.id });
        }
    }

    let doc =
        ingest_with_optional_provenance(db, storage, args, identity, provenance, bytes).await?;
    Ok(Filed::Revision(doc))
}

async fn ingest_with_optional_provenance(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    bytes: &[u8],
) -> Result<crate::documents::IngestedDocument, crate::documents::IngestError> {
    match provenance {
        Some(provenance) => {
            crate::documents::ingest_bytes_with_mail_provenance(
                db, storage, args, identity, provenance, bytes,
            )
            .await
        }
        None => crate::documents::ingest_bytes_as(db, storage, args, identity, bytes).await,
    }
}

/// Every asset in the database, for tests that count rows without a
/// project scope.
#[cfg(test)]
async fn for_project_free(db: &SurrealDb) -> Vec<Asset> {
    let response = db
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .unwrap();
    many(response).unwrap()
}

#[cfg(test)]
mod tests {
    use super::{
        count_for_projects, current, due_for_purge, fetch, find_by_id, for_project,
        for_project_including_deleted, grouped_for_project, ingest_content, kinds_by_projects,
        latest_of_kind, mark_purged, restore, revisions, revisions_for_slugs, set_legal_hold,
        sha256_hex, soft_delete, Asset, Lens, RestoreError, SoftDeleteError,
    };
    use crate::surreal::test_support::mem;
    use crate::surreal::SurrealDb;
    use chrono::{DateTime, Utc};
    use cloud::{FsStorage, StorageService};
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    async fn fixtures() -> (SurrealDb, Arc<dyn StorageService>, tempfile::TempDir) {
        let db = mem().await;
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageService> =
            Arc::new(FsStorage::new(tmp.path().to_path_buf()).await.unwrap());
        (db, storage, tmp)
    }

    #[tokio::test]
    async fn an_empty_revision_slug_batch_does_not_touch_the_database() {
        let db = SurrealDb::uninitialized();
        assert!(revisions_for_slugs(&db, Uuid::now_v7(), &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn ingest_content_writes_a_bare_asset_and_fetch_round_trips() {
        let (db, storage, _tmp) = fixtures().await;
        let id = ingest_content(&db, &storage, b"# a template body", "text/markdown")
            .await
            .unwrap();

        let row = find_by_id(&db, id).await.unwrap().unwrap();
        assert!(row.filename.is_none(), "bare content asset has no filename");
        assert!(row.project_id.is_none());
        assert!(row.source.is_none());
        assert_eq!(row.content_type, "text/markdown");
        assert!(row.storage_key.starts_with("blobs/"));

        let bytes = fetch(&db, &storage, id).await.unwrap();
        assert_eq!(bytes, b"# a template body");
    }

    #[tokio::test]
    async fn ingest_content_dedupes_identical_bare_content() {
        let (db, storage, _tmp) = fixtures().await;
        let a = ingest_content(&db, &storage, b"same", "text/markdown")
            .await
            .unwrap();
        let b = ingest_content(&db, &storage, b"same", "text/markdown")
            .await
            .unwrap();
        assert_eq!(a, b, "identical bare content reuses the same asset row");
        assert_eq!(super::for_project_free(&db).await.len(), 1);
    }

    /// The bare-content lane must never hand back a *document* asset that
    /// merely shares bytes — the two carry different metadata and a
    /// different expunge blast radius.
    #[tokio::test]
    async fn ingest_content_does_not_reuse_a_document_asset() {
        let (db, storage, _tmp) = fixtures().await;
        let bytes = b"shared bytes";
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        crate::documents::ingest_bytes(
            &db,
            &storage,
            &crate::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename: "doc.txt",
                kind: "unclassified",
                content_type: "text/plain",
                description: None,
                secondary_storage_key: None,
                visibility: crate::documents::visibility::INTERNAL,
            },
            bytes,
        )
        .await
        .unwrap();

        let bare = ingest_content(&db, &storage, bytes, "text/plain")
            .await
            .unwrap();
        let row = find_by_id(&db, bare).await.unwrap().unwrap();
        assert!(
            row.filename.is_none(),
            "bare-content ingest must create a fresh bare asset, not reuse the document asset"
        );
        assert_eq!(super::for_project_free(&db).await.len(), 2);
    }

    /// The headline risk of moving this table: `assets.metadata` is the
    /// ported JSONB column, and on a SCHEMAFULL table only `TYPE any`
    /// accepts every shape it carries. Each near miss fails differently
    /// and none of them fail at DEFINE time, so the round trip is pinned
    /// here rather than inferred from the schema file.
    #[tokio::test]
    async fn metadata_round_trips_every_json_shape() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;

        let shapes = [
            (
                "flat object",
                json!({ "backdated_reason": "court file stamp" }),
            ),
            (
                "nested object",
                json!({ "entry": { "sequence": 12, "court": "NV-8th" }, "sealed": false }),
            ),
            (
                "array",
                json!([{ "exhibit": "A" }, { "exhibit": "B", "pages": 4 }]),
            ),
            ("bare scalar", json!(42)),
        ];

        for (shape, metadata) in shapes {
            let doc = crate::documents::ingest_bytes_as(
                &db,
                &storage,
                &crate::documents::IngestArgs {
                    project_id: project,
                    source: "upload",
                    filename: "exhibit.pdf",
                    kind: "unclassified",
                    content_type: "application/pdf",
                    description: None,
                    secondary_storage_key: None,
                    visibility: crate::documents::visibility::INTERNAL,
                },
                &crate::documents::DocumentIdentity {
                    slug: None,
                    published_at: None,
                    metadata: Some(metadata.clone()),
                    derived_from: None,
                    transcript_quality: None,
                    signing: None,
                    notarization: Vec::new(),
                },
                format!("bytes for {shape}").as_bytes(),
            )
            .await
            .unwrap();

            let read_back = find_by_id(&db, doc.asset_id).await.unwrap().unwrap();
            assert_eq!(
                read_back.metadata.as_ref(),
                Some(&metadata),
                "the {shape} shape drifted through the engine"
            );
        }
    }

    /// A row with no metadata reads back as `None`, not as a JSON null —
    /// `TYPE any` carries no NOT NULL, so the absence has
    /// to stay distinguishable from a stored null.
    #[tokio::test]
    async fn absent_metadata_reads_back_as_none() {
        let (db, storage, _tmp) = fixtures().await;
        let id = ingest_content(&db, &storage, b"no metadata here", "text/plain")
            .await
            .unwrap();
        assert_eq!(find_by_id(&db, id).await.unwrap().unwrap().metadata, None);
    }

    #[tokio::test]
    async fn a_historical_asset_without_transcript_fields_still_reads() {
        let (db, storage, _tmp) = fixtures().await;
        let id = ingest_content(&db, &storage, b"pre-LAW-76 asset", "text/plain")
            .await
            .unwrap();
        db.query("UPDATE $id UNSET derived_from, transcript_quality")
            .bind(("id", crate::surreal::record_id(super::TABLE, id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        let row = find_by_id(&db, id).await.unwrap().unwrap();
        assert_eq!(row.derived_from, None);
        assert_eq!(row.transcript_quality, None);
    }

    #[tokio::test]
    async fn a_historical_asset_without_a_signing_record_still_reads() {
        let (db, storage, _tmp) = fixtures().await;
        let id = ingest_content(&db, &storage, b"unsigned asset", "text/plain")
            .await
            .unwrap();
        db.query("UPDATE $id UNSET signing")
            .bind(("id", crate::surreal::record_id(super::TABLE, id)))
            .await
            .unwrap()
            .check()
            .unwrap();

        assert_eq!(find_by_id(&db, id).await.unwrap().unwrap().signing, None);
    }

    #[tokio::test]
    async fn a_signing_record_round_trips_through_the_engine() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let signing = crate::documents::Signing {
            method: crate::documents::SigningMethod::WetInk,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
        };
        let ingested = crate::documents::ingest_bytes_as(
            &db,
            &storage,
            &crate::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename: "letter_signed.pdf",
                kind: "onboarding",
                content_type: "application/pdf",
                description: None,
                secondary_storage_key: None,
                visibility: crate::documents::visibility::INTERNAL,
            },
            &crate::documents::DocumentIdentity {
                slug: Some("onboarding/letter_signed.pdf"),
                signing: Some(signing),
                ..Default::default()
            },
            b"signed scan",
        )
        .await
        .unwrap();

        let row = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert_eq!(row.signing, Some(signing));
    }

    #[tokio::test]
    async fn notarizations_round_trip_and_a_historical_row_reads_as_none() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let notarization = vec![
            crate::documents::Notarization {
                method: crate::documents::NotarizationMethod::InPerson,
                acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 4).unwrap(),
                state: "NV".into(),
                county: "Clark".into(),
                commission_expires: chrono::NaiveDate::from_ymd_opt(2029, 1, 31).unwrap(),
            },
            crate::documents::Notarization {
                method: crate::documents::NotarizationMethod::RemoteOnline,
                acknowledged_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 5).unwrap(),
                state: "CA".into(),
                county: "Los Angeles".into(),
                commission_expires: chrono::NaiveDate::from_ymd_opt(2027, 6, 30).unwrap(),
            },
        ];
        let ingested = crate::documents::ingest_bytes_as(
            &db,
            &storage,
            &crate::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename: "will_signed.pdf",
                kind: "will",
                content_type: "application/pdf",
                description: None,
                secondary_storage_key: None,
                visibility: crate::documents::visibility::INTERNAL,
            },
            &crate::documents::DocumentIdentity {
                slug: Some("will_signed.pdf"),
                notarization: notarization.clone(),
                ..Default::default()
            },
            b"notarized scan",
        )
        .await
        .unwrap();
        let row = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert_eq!(row.notarization, notarization);

        db.query("UPDATE $id UNSET notarization")
            .bind((
                "id",
                crate::surreal::record_id(super::TABLE, ingested.asset_id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        let historical = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert!(historical.notarization.is_empty());
    }

    #[tokio::test]
    async fn a_missing_asset_reads_back_as_none() {
        let db = mem().await;
        assert!(find_by_id(&db, uuid::Uuid::now_v7())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn sha256_hex_matches_the_known_digest() {
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    /// Insertion order decides which revision is operative, and a
    /// back-dated `published_at` must not reorder the chain.
    #[tokio::test]
    async fn insertion_order_decides_current_not_published_at() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let file = |bytes: &'static [u8], published: &'static str| {
            let db = db.clone();
            let storage = storage.clone();
            async move {
                crate::documents::ingest_bytes_as(
                    &db,
                    &storage,
                    &crate::documents::IngestArgs {
                        project_id: project,
                        source: "upload",
                        filename: "agreement.pdf",
                        kind: "agreement",
                        content_type: "application/pdf",
                        description: None,
                        secondary_storage_key: None,
                        visibility: crate::documents::visibility::CLIENT,
                    },
                    &crate::documents::DocumentIdentity {
                        slug: Some("agreement"),
                        published_at: Some(published),
                        metadata: None,
                        derived_from: None,
                        transcript_quality: None,
                        signing: None,
                        notarization: Vec::new(),
                    },
                    bytes,
                )
                .await
                .unwrap()
            }
        };

        let first = file(b"v1", "2026-06-01T00:00:00Z").await;
        // The second revision is *back-dated* below the first.
        let second = file(b"v2", "2020-01-01T00:00:00Z").await;

        let operative = super::current(&db, project, "agreement", super::Lens::Lawyer)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            operative.id, second.asset_id,
            "the later insertion is current even though it is back-dated"
        );

        let chain: Vec<_> = super::revisions(&db, project, "agreement")
            .await
            .unwrap()
            .into_iter()
            .map(|a: Asset| a.id)
            .collect();
        assert_eq!(chain, vec![second.asset_id, first.asset_id]);
    }

    /// The client lens skips an unpublished or lawyer-only revision sitting
    /// above the one they executed.
    #[tokio::test]
    async fn the_client_lens_stops_at_the_latest_published_client_revision() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let file = |bytes: &'static [u8], published: Option<&'static str>, vis: &'static str| {
            let db = db.clone();
            let storage = storage.clone();
            async move {
                crate::documents::ingest_bytes_as(
                    &db,
                    &storage,
                    &crate::documents::IngestArgs {
                        project_id: project,
                        source: "upload",
                        filename: "agreement.pdf",
                        kind: "agreement",
                        content_type: "application/pdf",
                        description: None,
                        secondary_storage_key: None,
                        visibility: vis,
                    },
                    &crate::documents::DocumentIdentity {
                        slug: Some("agreement"),
                        published_at: published,
                        metadata: None,
                        derived_from: None,
                        transcript_quality: None,
                        signing: None,
                        notarization: Vec::new(),
                    },
                    bytes,
                )
                .await
                .unwrap()
            }
        };

        let executed = file(
            b"executed",
            Some("2026-06-01T00:00:00Z"),
            crate::documents::visibility::CLIENT,
        )
        .await;
        // Lawyers keep working above it: an unpublished redline…
        let redline = file(b"redline", None, crate::documents::visibility::CLIENT).await;

        assert_eq!(
            super::current(&db, project, "agreement", super::Lens::Lawyer)
                .await
                .unwrap()
                .unwrap()
                .id,
            redline.asset_id,
            "lawyers see the redline they are still working on"
        );
        assert_eq!(
            super::current(&db, project, "agreement", super::Lens::Client)
                .await
                .unwrap()
                .unwrap()
                .id,
            executed.asset_id,
            "the client stays anchored to the executed revision"
        );
    }

    async fn ingest_named(
        db: &SurrealDb,
        storage: &Arc<dyn StorageService>,
        project: uuid::Uuid,
        filename: &str,
        kind: &str,
        slug: Option<&str>,
        bytes: &[u8],
    ) -> Asset {
        let ingested = crate::documents::ingest_bytes_as(
            db,
            storage,
            &crate::documents::IngestArgs {
                project_id: project,
                source: "upload",
                filename,
                kind,
                content_type: "text/plain",
                description: None,
                secondary_storage_key: None,
                visibility: crate::documents::visibility::INTERNAL,
            },
            &crate::documents::DocumentIdentity {
                slug,
                published_at: None,
                metadata: None,
                derived_from: None,
                transcript_quality: None,
                signing: None,
                notarization: Vec::new(),
            },
            bytes,
        )
        .await
        .unwrap();
        find_by_id(db, ingested.asset_id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn assign_slug_sets_slug_and_kind_without_touching_bytes() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let before = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            b"slugless bytes",
        )
        .await;

        let after = super::assign_slug(
            &db,
            project,
            before.id,
            super::AssignSlug {
                slug: "notes/note.txt",
                kind: Some("exhibit"),
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(after.slug.as_deref(), Some("notes/note.txt"));
        assert_eq!(after.kind.as_deref(), Some("exhibit"));
        assert_eq!(after.sha256_hex, before.sha256_hex);
        assert_eq!(after.storage_key, before.storage_key);
        assert_eq!(after.byte_size, before.byte_size);
        assert_eq!(after.id, before.id);
    }

    #[tokio::test]
    async fn assign_slug_refuses_a_row_that_already_has_a_slug() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("notes/note.txt"),
            b"already slugged",
        )
        .await;

        let err = super::assign_slug(
            &db,
            project,
            row.id,
            super::AssignSlug {
                slug: "other/note.txt",
                kind: None,
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, super::AssignSlugError::AlreadySlugged));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.slug.as_deref(), Some("notes/note.txt"));
    }

    #[tokio::test]
    async fn assign_slug_refuses_a_slug_that_already_has_a_chain() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("notes/note.txt"),
            b"chain head",
        )
        .await;
        let slugless = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            b"a different document",
        )
        .await;

        let err = super::assign_slug(
            &db,
            project,
            slugless.id,
            super::AssignSlug {
                slug: "notes/note.txt",
                kind: Some("memo"),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, super::AssignSlugError::ChainExists(_)));
        let stored = find_by_id(&db, slugless.id).await.unwrap().unwrap();
        assert!(stored.slug.is_none());
        assert_eq!(stored.kind.as_deref(), Some("unclassified"));
    }

    #[tokio::test]
    async fn assign_slug_refuses_a_kind_outside_the_asset_lane() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            b"kind check",
        )
        .await;

        let err = super::assign_slug(
            &db,
            project,
            row.id,
            super::AssignSlug {
                slug: "notes/note.txt",
                kind: Some("review_queue_workbench"),
                dry_run: false,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, super::AssignSlugError::InvalidKind(_)));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert!(stored.slug.is_none());
        assert_eq!(stored.kind.as_deref(), Some("unclassified"));
    }

    async fn force_kind(db: &SurrealDb, asset_id: uuid::Uuid, kind: &str) {
        db.query("UPDATE $id SET kind = $kind")
            .bind(("id", super::record_id(super::TABLE, asset_id)))
            .bind(("kind", kind.to_string()))
            .await
            .unwrap()
            .check()
            .unwrap();
    }

    #[tokio::test]
    async fn reclassify_kind_rewrites_a_legacy_kind_across_the_chain() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let first = ingest_named(
            &db,
            &storage,
            project,
            "filing.txt",
            "unclassified",
            Some("filing.txt"),
            b"first revision",
        )
        .await;
        let second = ingest_named(
            &db,
            &storage,
            project,
            "filing.txt",
            "unclassified",
            Some("filing.txt"),
            b"second revision",
        )
        .await;
        force_kind(&db, first.id, "Formation Filing").await;
        force_kind(&db, second.id, "Formation Filing").await;

        let preview = super::reclassify_kind(&db, project, second.id, "filing", true)
            .await
            .unwrap();
        assert_eq!(preview.kind.as_deref(), Some("filing"));
        let untouched = find_by_id(&db, second.id).await.unwrap().unwrap();
        assert_eq!(untouched.kind.as_deref(), Some("Formation Filing"));

        let written = super::reclassify_kind(&db, project, second.id, "filing", false)
            .await
            .unwrap();
        assert_eq!(written.kind.as_deref(), Some("filing"));
        assert_eq!(written.slug, second.slug);
        assert_eq!(written.sha256_hex, second.sha256_hex);
        assert_eq!(written.storage_key, second.storage_key);
        let sibling = find_by_id(&db, first.id).await.unwrap().unwrap();
        assert_eq!(sibling.kind.as_deref(), Some("filing"));
    }

    #[tokio::test]
    async fn reclassify_kind_refuses_a_row_whose_kind_is_accepted() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "memo.txt",
            "memo",
            Some("memo.txt"),
            b"a valid memo",
        )
        .await;

        let err = super::reclassify_kind(&db, project, row.id, "letter", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReclassifyKindError::KindAccepted(_)));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.kind.as_deref(), Some("memo"));
    }

    #[tokio::test]
    async fn reclassify_kind_refuses_a_target_outside_the_asset_lane() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "receipt.txt",
            "unclassified",
            Some("receipt.txt"),
            b"legacy receipt",
        )
        .await;
        force_kind(&db, row.id, "Receipt").await;

        let err = super::reclassify_kind(&db, project, row.id, "review_queue_workbench", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReclassifyKindError::InvalidKind(_)));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.kind.as_deref(), Some("Receipt"));
    }

    #[tokio::test]
    async fn reclassify_kind_refuses_a_chain_with_an_accepted_revision() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let valid = ingest_named(
            &db,
            &storage,
            project,
            "contract.txt",
            "agreement",
            Some("contract.txt"),
            b"valid revision",
        )
        .await;
        let legacy = ingest_named(
            &db,
            &storage,
            project,
            "contract.txt",
            "agreement",
            Some("contract.txt"),
            b"legacy revision",
        )
        .await;
        force_kind(&db, legacy.id, "Consulting Contract").await;

        let err = super::reclassify_kind(&db, project, legacy.id, "inbound_contract", false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            super::ReclassifyKindError::ChainHasAcceptedKind(_)
        ));
        let stored = find_by_id(&db, legacy.id).await.unwrap().unwrap();
        assert_eq!(stored.kind.as_deref(), Some("Consulting Contract"));
        let head = find_by_id(&db, valid.id).await.unwrap().unwrap();
        assert_eq!(head.kind.as_deref(), Some("agreement"));
    }

    #[tokio::test]
    async fn reclassify_accepted_kind_rewrites_every_revision_without_touching_bytes() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let first = ingest_named(
            &db,
            &storage,
            project,
            "draft.md",
            "memo",
            Some("drafts/draft.md"),
            b"first draft",
        )
        .await;
        let second = ingest_named(
            &db,
            &storage,
            project,
            "draft.md",
            "memo",
            Some("drafts/draft.md"),
            b"second draft",
        )
        .await;

        let preview =
            super::reclassify_accepted_kind(&db, project, second.id, "unclassified", true)
                .await
                .unwrap();
        assert_eq!(preview.kind.as_deref(), Some("unclassified"));
        let untouched = find_by_id(&db, second.id).await.unwrap().unwrap();
        assert_eq!(untouched.kind.as_deref(), Some("memo"));

        let written =
            super::reclassify_accepted_kind(&db, project, second.id, "unclassified", false)
                .await
                .unwrap();
        assert_eq!(written.kind.as_deref(), Some("unclassified"));
        for (before, id) in [(&first, first.id), (&second, second.id)] {
            let stored = find_by_id(&db, id).await.unwrap().unwrap();
            assert_eq!(stored.kind.as_deref(), Some("unclassified"));
            assert_eq!(stored.slug, before.slug);
            assert_eq!(stored.sha256_hex, before.sha256_hex);
            assert_eq!(stored.storage_key, before.storage_key);
            assert_eq!(stored.byte_size, before.byte_size);
        }
    }

    #[tokio::test]
    async fn reclassify_accepted_kind_refuses_a_legacy_kind_a_same_kind_and_a_bad_target() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let other = crate::test_support::seed_project_surreal(&db, "other").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "memo.txt",
            "memo",
            Some("memo.txt"),
            b"a valid memo",
        )
        .await;

        let err = super::reclassify_accepted_kind(&db, other, row.id, "letter", false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            super::ReclassifyAcceptedKindError::NotOnProject
        ));
        let err = super::reclassify_accepted_kind(&db, project, row.id, "memo", false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            super::ReclassifyAcceptedKindError::KindUnchanged(_)
        ));
        let err =
            super::reclassify_accepted_kind(&db, project, row.id, "review_queue_workbench", false)
                .await
                .unwrap_err();
        assert!(matches!(
            err,
            super::ReclassifyAcceptedKindError::InvalidKind(_)
        ));

        force_kind(&db, row.id, "Legacy Memo").await;
        let err = super::reclassify_accepted_kind(&db, project, row.id, "letter", false)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            super::ReclassifyAcceptedKindError::KindNotAccepted(_)
        ));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.kind.as_deref(), Some("Legacy Memo"));
    }

    #[tokio::test]
    async fn reslug_moves_a_bare_legacy_slug_and_its_chain_without_touching_bytes() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        // Legacy chain: a bare slug with no extension of its own, set before
        // slugs had to carry the source filename's extension (LAW-26).
        let first = ingest_named(
            &db,
            &storage,
            project,
            "summons-wendell-prine.pdf",
            "filing",
            Some("summons-wendell-prine"),
            b"first revision",
        )
        .await;
        let second = ingest_named(
            &db,
            &storage,
            project,
            "summons-wendell-prine.pdf",
            "filing",
            Some("summons-wendell-prine"),
            b"second revision",
        )
        .await;

        let preview = super::reslug(&db, project, second.id, "summons-wendell-prine.pdf", true)
            .await
            .unwrap();
        assert_eq!(preview.slug.as_deref(), Some("summons-wendell-prine.pdf"));
        let untouched = find_by_id(&db, second.id).await.unwrap().unwrap();
        assert_eq!(untouched.slug.as_deref(), Some("summons-wendell-prine"));

        let written = super::reslug(&db, project, second.id, "summons-wendell-prine.pdf", false)
            .await
            .unwrap();
        assert_eq!(written.slug.as_deref(), Some("summons-wendell-prine.pdf"));
        assert_eq!(written.sha256_hex, second.sha256_hex);
        assert_eq!(written.storage_key, second.storage_key);
        assert_eq!(written.byte_size, second.byte_size);
        assert_eq!(written.kind, second.kind);
        let sibling = find_by_id(&db, first.id).await.unwrap().unwrap();
        assert_eq!(
            sibling.slug.as_deref(),
            Some("summons-wendell-prine.pdf"),
            "the whole chain moves together, not only the requested revision"
        );
    }

    #[tokio::test]
    async fn reslug_moves_a_valid_identity_and_refuses_a_no_op() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("notes/note.txt"),
            b"already a valid identity",
        )
        .await;

        let err = super::reslug(&db, project, row.id, "notes/note.txt", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReslugError::SlugUnchanged(_)));

        let moved = super::reslug(&db, project, row.id, "filed/forms/note.txt", false)
            .await
            .unwrap();
        assert_eq!(moved.slug.as_deref(), Some("filed/forms/note.txt"));
        assert_eq!(moved.sha256_hex, row.sha256_hex);
        assert_eq!(moved.storage_key, row.storage_key);
    }

    #[tokio::test]
    async fn reslug_refuses_a_row_with_no_slug() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            b"slugless",
        )
        .await;

        let err = super::reslug(&db, project, row.id, "notes/note.txt", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReslugError::NoSlug));
    }

    #[tokio::test]
    async fn reslug_refuses_an_occupied_target() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("notes/note.txt"),
            b"already occupies the target",
        )
        .await;
        let legacy = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("note"),
            b"a different document",
        )
        .await;

        let err = super::reslug(&db, project, legacy.id, "notes/note.txt", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReslugError::TargetOccupied(_)));
        let stored = find_by_id(&db, legacy.id).await.unwrap().unwrap();
        assert_eq!(stored.slug.as_deref(), Some("note"));
    }

    #[tokio::test]
    async fn reslug_refuses_a_target_missing_the_source_extension() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("note"),
            b"legacy bare slug",
        )
        .await;

        let err = super::reslug(&db, project, row.id, "renamed-note", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReslugError::InvalidTarget(_)));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.slug.as_deref(), Some("note"));
    }

    #[tokio::test]
    async fn reslug_refuses_an_asset_on_another_project() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let other_project = crate::test_support::seed_project_surreal(&db, "other-matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            Some("note"),
            b"cross project",
        )
        .await;

        let err = super::reslug(&db, other_project, row.id, "notes/note.txt", false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::ReslugError::NotOnProject));
    }

    #[tokio::test]
    async fn repair_copies_a_sibling_object_onto_the_missing_key() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let bytes = b"shared document bytes";
        let broken = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            bytes,
        )
        .await;
        let holder = ingest_named(
            &db,
            &storage,
            project,
            "copy.txt",
            "unclassified",
            Some("copies/copy.txt"),
            bytes,
        )
        .await;
        let sibling_key = format!("{}-held", broken.storage_key);
        storage
            .put(&sibling_key, bytes, "text/plain")
            .await
            .unwrap();
        super::writing(|| {
            db.query("UPDATE $id SET storage_key = $storage_key")
                .bind(("id", crate::surreal::record_id(super::TABLE, holder.id)))
                .bind(("storage_key", sibling_key.clone()))
        })
        .await
        .unwrap();
        storage.delete(&broken.storage_key).await.unwrap();

        let repaired =
            super::repair_missing_object(&db, storage.as_ref(), project, broken.id, false)
                .await
                .unwrap();
        assert!(repaired.copied);
        assert_eq!(repaired.storage_key, broken.storage_key);
        let stored = find_by_id(&db, broken.id).await.unwrap().unwrap();
        assert_eq!(stored.storage_key, broken.storage_key);
        assert_eq!(stored.sha256_hex, broken.sha256_hex);
        assert_eq!(
            super::object_sha256(storage.as_ref(), &stored.storage_key)
                .await
                .unwrap()
                .as_deref(),
            Some(stored.sha256_hex.as_str())
        );
    }

    #[tokio::test]
    async fn repair_refuses_when_no_sibling_holds_the_content() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let row = ingest_named(
            &db,
            &storage,
            project,
            "note.txt",
            "unclassified",
            None,
            b"only copy",
        )
        .await;
        storage.delete(&row.storage_key).await.unwrap();

        let err = super::repair_missing_object(&db, storage.as_ref(), project, row.id, false)
            .await
            .unwrap_err();
        assert!(matches!(err, super::RepairStorageError::NoSibling));
        let stored = find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(stored.storage_key, row.storage_key);
        assert_eq!(stored.sha256_hex, row.sha256_hex);
        assert!(super::object_head(storage.as_ref(), &row.storage_key)
            .await
            .unwrap()
            .is_none());
    }

    async fn lawyer(db: &SurrealDb) -> Uuid {
        crate::persons::create(
            db,
            &crate::persons::NewPerson::new("Nick", "nick@neonlaw.com"),
        )
        .await
        .unwrap()
        .id
    }

    /// A two-revision document, newest last.
    async fn two_revisions(
        db: &SurrealDb,
        storage: &Arc<dyn StorageService>,
        project: Uuid,
    ) -> (Asset, Asset) {
        let first = ingest_named(
            db,
            storage,
            project,
            "memo.txt",
            "memo",
            Some("memos/memo.txt"),
            b"first",
        )
        .await;
        let second = ingest_named(
            db,
            storage,
            project,
            "memo.txt",
            "memo",
            Some("memos/memo.txt"),
            b"second",
        )
        .await;
        (first, second)
    }

    fn at(day: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 10, day, 12, 0, 0).unwrap()
    }

    /// ENG-862: a soft delete stamps the whole chain with who, which commit,
    /// and the purge date 180 days out, and drops it from every default read
    /// while a lawyer can still read it.
    #[tokio::test]
    async fn soft_delete_stamps_the_chain_and_hides_it_from_default_reads() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, second) = two_revisions(&db, &storage, project).await;
        let person = lawyer(&db).await;

        let deletion = soft_delete(&db, project, first.id, person, Some("abc1234"), at(2))
            .await
            .unwrap();

        assert!(deletion.changed);
        assert_eq!(deletion.revisions, 2);
        assert_eq!(deletion.deleted_at, at(2));
        assert_eq!(deletion.purge_after, at(2) + chrono::Duration::days(180));
        for id in [first.id, second.id] {
            let row = find_by_id(&db, id).await.unwrap().unwrap();
            assert_eq!(row.deleted_at, Some(at(2)));
            assert_eq!(row.purge_after, Some(deletion.purge_after));
            assert_eq!(row.deleted_by_person_id, Some(person));
            assert_eq!(row.deleted_commit_sha.as_deref(), Some("abc1234"));
        }
        assert!(for_project(&db, project).await.unwrap().is_empty());
        assert!(grouped_for_project(&db, project).await.unwrap().is_empty());
        assert!(current(&db, project, "memos/memo.txt", Lens::Lawyer)
            .await
            .unwrap()
            .is_none());
        assert!(current(&db, project, "memos/memo.txt", Lens::Client)
            .await
            .unwrap()
            .is_none());
        assert!(latest_of_kind(&db, project, "memo")
            .await
            .unwrap()
            .is_none());
        assert_eq!(count_for_projects(&db, &[project]).await.unwrap(), 0);
        assert!(kinds_by_projects(&db, &[project]).await.unwrap().is_empty());
        // A lawyer still reads the chain, in full.
        assert_eq!(
            revisions(&db, project, "memos/memo.txt")
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            for_project_including_deleted(&db, project)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_direct_retirement_hides_the_chain_without_a_repository_commit() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, second) = two_revisions(&db, &storage, project).await;
        let person = lawyer(&db).await;

        let deletion = soft_delete(&db, project, first.id, person, None, at(2))
            .await
            .unwrap();

        assert!(deletion.changed);
        assert_eq!(deletion.revisions, 2);
        for id in [first.id, second.id] {
            let row = find_by_id(&db, id).await.unwrap().unwrap();
            assert_eq!(row.deleted_by_person_id, Some(person));
            assert!(row.deleted_at.is_some());
            assert!(row.deleted_commit_sha.is_none());
        }
        assert!(for_project(&db, project).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn deleting_a_deleted_chain_again_keeps_the_original_stamps() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, _) = two_revisions(&db, &storage, project).await;
        let person = lawyer(&db).await;
        soft_delete(&db, project, first.id, person, Some("abc1234"), at(2))
            .await
            .unwrap();

        let again = soft_delete(&db, project, first.id, person, Some("def5678"), at(9))
            .await
            .unwrap();

        assert!(!again.changed);
        assert_eq!(again.deleted_at, at(2), "the first deletion's stamps stand");
        let row = find_by_id(&db, first.id).await.unwrap().unwrap();
        assert_eq!(row.deleted_commit_sha.as_deref(), Some("abc1234"));
    }

    #[tokio::test]
    async fn a_closed_repository_and_a_foreign_row_are_never_soft_deleted() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let other = crate::test_support::seed_project_surreal(&db, "other").await;
        let archive = ingest_named(
            &db,
            &storage,
            project,
            "matter-closed-repository.zip",
            "closed_repository",
            Some("closed_repository.zip"),
            b"zip",
        )
        .await;
        let person = lawyer(&db).await;

        assert!(matches!(
            soft_delete(&db, project, archive.id, person, Some("abc1234"), at(2)).await,
            Err(SoftDeleteError::ClosedRepository)
        ));
        assert!(matches!(
            soft_delete(&db, other, archive.id, person, Some("abc1234"), at(2)).await,
            Err(SoftDeleteError::NotOnProject)
        ));
        assert!(find_by_id(&db, archive.id)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_none());
    }

    /// Restoring within the window returns the document and its full revision
    /// history to every default read.
    #[tokio::test]
    async fn restore_returns_the_chain_and_its_history() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, second) = two_revisions(&db, &storage, project).await;
        let person = lawyer(&db).await;
        soft_delete(&db, project, second.id, person, Some("abc1234"), at(2))
            .await
            .unwrap();

        let restored = restore(&db, project, first.id).await.unwrap();

        assert!(restored.changed);
        assert_eq!(restored.revisions, 2);
        let live = for_project(&db, project).await.unwrap();
        assert_eq!(live.len(), 2);
        assert!(live.iter().all(|row| row.deleted_at.is_none()
            && row.purge_after.is_none()
            && row.deleted_by_person_id.is_none()
            && row.deleted_commit_sha.is_none()));
        assert_eq!(
            current(&db, project, "memos/memo.txt", Lens::Lawyer)
                .await
                .unwrap()
                .unwrap()
                .id,
            second.id
        );
        assert!(!restore(&db, project, first.id).await.unwrap().changed);
    }

    #[tokio::test]
    async fn a_purged_chain_cannot_be_restored() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, second) = two_revisions(&db, &storage, project).await;
        let person = lawyer(&db).await;
        soft_delete(&db, project, first.id, person, Some("abc1234"), at(2))
            .await
            .unwrap();
        mark_purged(&db, &[first.id, second.id], at(9))
            .await
            .unwrap();

        assert!(matches!(
            restore(&db, project, first.id).await,
            Err(RestoreError::Purged)
        ));
    }

    /// Retention: only rows past `purge_after`, and never one already purged.
    #[tokio::test]
    async fn due_for_purge_lists_only_rows_past_their_window_that_are_not_purged() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let person = lawyer(&db).await;
        let old = ingest_named(
            &db,
            &storage,
            project,
            "old.txt",
            "memo",
            Some("old.txt"),
            b"old",
        )
        .await;
        let recent = ingest_named(
            &db,
            &storage,
            project,
            "recent.txt",
            "memo",
            Some("recent.txt"),
            b"recent",
        )
        .await;
        let live = ingest_named(
            &db,
            &storage,
            project,
            "live.txt",
            "memo",
            Some("live.txt"),
            b"live",
        )
        .await;
        soft_delete(&db, project, old.id, person, Some("abc1234"), at(1))
            .await
            .unwrap();
        soft_delete(
            &db,
            project,
            recent.id,
            person,
            Some("abc1234"),
            at(1) + chrono::Duration::days(100),
        )
        .await
        .unwrap();
        let after_window = at(1) + chrono::Duration::days(181);

        let due = due_for_purge(&db, after_window).await.unwrap();

        assert_eq!(
            due.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![old.id]
        );
        assert!(!due.iter().any(|row| row.id == live.id));
        let exactly = at(1) + chrono::Duration::days(180);
        assert_eq!(
            due_for_purge(&db, exactly).await.unwrap().len(),
            1,
            "the boundary is due"
        );
        assert!(due_for_purge(&db, exactly - chrono::Duration::seconds(1))
            .await
            .unwrap()
            .is_empty());
        mark_purged(&db, &[old.id], after_window).await.unwrap();
        assert!(due_for_purge(&db, after_window).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_legal_hold_is_placed_and_released_on_the_whole_chain() {
        let (db, storage, _tmp) = fixtures().await;
        let project = crate::test_support::seed_project_surreal(&db, "matter").await;
        let (first, second) = two_revisions(&db, &storage, project).await;

        assert_eq!(
            set_legal_hold(&db, project, first.id, true, at(2))
                .await
                .unwrap(),
            Some(2)
        );
        for id in [first.id, second.id] {
            assert_eq!(
                find_by_id(&db, id).await.unwrap().unwrap().legal_hold_at,
                Some(at(2))
            );
        }
        assert_eq!(
            set_legal_hold(&db, project, second.id, false, at(3))
                .await
                .unwrap(),
            Some(2)
        );
        assert!(find_by_id(&db, first.id)
            .await
            .unwrap()
            .unwrap()
            .legal_hold_at
            .is_none());
        let elsewhere = crate::test_support::seed_project_surreal(&db, "other").await;
        assert_eq!(
            set_legal_hold(&db, elsewhere, first.id, true, at(2))
                .await
                .unwrap(),
            None
        );
    }
}
