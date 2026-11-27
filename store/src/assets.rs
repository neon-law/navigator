//! `store::assets` — the content-addressed lane for the `asset` table,
//! and every query against it.
//!
//! # PostgreSQL authority
//!
//! PostgreSQL owns Asset metadata after verified Notation-family enrollment.
//! Reads and writes fail closed while configured PostgreSQL awaits that proof.
//! The compatibility lane runs only without configured PostgreSQL.
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

use crate::postgres::{assets as native, matter};
use serde_json::json;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
pub use model::documents::sha256_hex;

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
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::postgres::matter::present_json"
    )]
    pub metadata: Option<Json>,
    /// Source revision for a generated transcript, if this row derives from
    /// another Project document. Optional for rows written before LAW-76.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::postgres::matter::present_json"
    )]
    pub derived_from: Option<Json>,
    /// `machine` or `proofread` when transcript quality is known; absent for
    /// legacy and user-authored transcripts without an explicit assertion.
    pub transcript_quality: Option<String>,
    /// How an executed document was signed, when that was recorded at upload.
    pub signing: Option<crate::documents::Signing>,
    /// The notarial acknowledgments recorded at upload; empty when none were.
    #[serde(default)]
    pub notarization: Vec<crate::documents::Notarization>,
    /// Evidence provenance a lawyer recorded at upload (LAW-185): the date
    /// the exhibit itself bears, where it came from, how confidently it was
    /// identified, and the claim references it supports. `None` is
    /// unrecorded; a stored value this build cannot read reads back as
    /// unrecorded rather than failing the row.
    pub evidence: Option<crate::documents::Evidence>,
    /// The matter folder the document is filed in (LAW-241) — the pointer's
    /// `folder:` key, spelled exactly, persisted so the live matter files
    /// the document where the pointer says. `None` is unrecorded: the
    /// document sits where its kind's default placement puts it. A stored
    /// value this build cannot read reads back as unrecorded rather than
    /// failing the row.
    pub folder: Option<String>,
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

impl Asset {
    /// Explicit checkout placement of a provider import, independent of its slug.
    #[must_use]
    pub fn drive_checkout_path(&self) -> Option<&str> {
        self.metadata.as_ref()?.get("drive_checkout_path")?.as_str()
    }

    /// Where this document sits below a Project checkout's `documents/`, by
    /// the same rule `navigator project sync` lays it out with: the slug, or
    /// the filename of a one-off, in the matter folder the row's persisted
    /// `folder` names (LAW-241) — the pointer's `folder:` key — else the
    /// one its kind maps to.
    #[must_use]
    pub fn documents_path(&self) -> String {
        let name = self
            .slug
            .as_deref()
            .or(self.filename.as_deref())
            .unwrap_or_default();
        if let Some(path) = self.drive_checkout_path() {
            return path.to_owned();
        }
        let folder = self
            .folder
            .as_deref()
            .and_then(crate::document_pointers::MatterFolder::parse)
            .unwrap_or_else(|| {
                crate::document_pointers::MatterFolder::for_kind(
                    self.kind.as_deref().unwrap_or_default(),
                )
            });
        crate::document_pointers::documents_path(name, folder)
    }
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

/// Errors from [`ingest_content`] / [`fetch`].
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    #[error("the asset's Notation association changed concurrently")]
    ConcurrentAssociation,
    #[error("PostgreSQL asset operation failed")]
    PostgresQuery(#[from] sqlx::Error),
    #[error(transparent)]
    Authority(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("storage: {0}")]
    Storage(#[from] cloud::StorageError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back — see [`AssetRow::into_asset`].
    #[error("writing an asset returned no usable row")]
    WriteReturnedNothing,
    #[error("PostgreSQL asset metadata: {0}")]
    Postgres(String),
}

async fn patch_postgres(id: Uuid, changes: Json) -> Result<(), AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::patch(&pg, id, changes)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    Ok(())
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

/// Resolve an asset by id.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(
        native::rows(&pg, "AND r.id=($1->>'id')::uuid", json!({"id":id}))
            .await?
            .into_iter()
            .next(),
    )
}

/// Resolve many assets by id in one round trip — what a listing does after
/// reading a page of rows that reference them, rather than one lookup per
/// row. Ids that match nothing are simply absent.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_ids(_db: &ApplicationStore, ids: &[Uuid]) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(
        &pg,
        "AND r.id IN (SELECT jsonb_array_elements_text($1->'ids')::uuid)",
        json!({"ids":ids}),
    )
    .await
}

/// Every asset on a Project, newest first — the lawyer document listing.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(
        &pg,
        "AND r.project_id=($1->>'project')::uuid AND r.deleted_at IS NULL ORDER BY id DESC",
        json!({"project":project_id}),
    )
    .await
}

/// The metadata used to display or download a Project Inbox attachment.
#[derive(Debug)]
pub struct AttachmentMetadata {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub filename: Option<String>,
    pub kind: Option<String>,
    pub content_type: String,
    pub storage_key: String,
}

impl AttachmentMetadata {
    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            project_id: row.try_get("project_id")?,
            filename: row.try_get("filename")?,
            kind: row.try_get("kind")?,
            content_type: row.try_get("content_type")?,
            storage_key: row.try_get("storage_key")?,
        })
    }
}

impl From<Asset> for AttachmentMetadata {
    fn from(asset: Asset) -> Self {
        Self {
            id: asset.id,
            project_id: asset.project_id,
            filename: asset.filename,
            kind: asset.kind,
            content_type: asset.content_type,
            storage_key: asset.storage_key,
        }
    }
}

/// Metadata for an already-authorized Inbox attachment download.
pub async fn inbox_attachment_metadata(
    _db: &ApplicationStore,
    asset_id: Uuid,
) -> Result<Option<AttachmentMetadata>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    sqlx::query(
        "SELECT id, project_id, filename, kind, content_type, storage_key FROM asset WHERE id = $1",
    )
    .bind(asset_id)
    .try_map(|row| AttachmentMetadata::from_row(&row))
    .fetch_optional(pg.pool())
    .await
    .map_err(|error| AssetError::Postgres(error.to_string()))
}

/// Resolve metadata for a Project Inbox Message's authorized attachment IDs
/// and older filed revisions. The primary slot precedes the filed revisions.
pub async fn project_inbox_attachments(
    _db: &ApplicationStore,
    project_id: Uuid,
    message_id: Uuid,
    primary: Option<Uuid>,
    linked: &[crate::inbox::MessageAttachment],
) -> Result<Vec<AttachmentMetadata>, AssetError> {
    let ids: Vec<_> = primary
        .into_iter()
        .chain(linked.iter().map(|link| link.asset_id))
        .collect();
    let source = message_id.to_string();

    let pg = matter::authority_for(TABLE).await?;
    // A linked ID can name a bare Asset or one on this Project. Only
    // live Project files may enter through the older provenance slot.
    let mut rows: Vec<(AttachmentMetadata, Option<String>)> = sqlx::query(
        "SELECT id, project_id, filename, kind, content_type, storage_key, slug FROM asset
         WHERE (id = ANY($1) AND (project_id IS NULL OR project_id = $2))
            OR (project_id = $2 AND source_message_id = $3 AND deleted_at IS NULL)",
    )
    .bind(&ids)
    .bind(project_id)
    .bind(&source)
    .try_map(|row| {
        use sqlx::Row;
        Ok((AttachmentMetadata::from_row(&row)?, row.try_get("slug")?))
    })
    .fetch_all(pg.pool())
    .await
    .map_err(|error: sqlx::Error| AssetError::Postgres(error.to_string()))?;
    rows.sort_by_key(|(asset, slug)| {
        (
            Some(asset.id) != primary,
            mail_attachment_index(slug.as_deref()),
            asset.id,
        )
    });
    Ok(rows.into_iter().map(|(asset, _)| asset).collect())
}

fn mail_attachment_index(slug: Option<&str>) -> u64 {
    slug.and_then(|slug| slug.rsplit('/').next())
        .and_then(|index| index.parse().ok())
        .unwrap_or(u64::MAX)
}

/// The attachments one Message filed onto a Project, in attachment order —
/// the `mail/{message_id}/{index}` revisions stamped with
/// [`crate::documents::MailProvenance`] (LAW-238 D1). Project-scoped: a
/// matching `source_message_id` on another matter never enters the list.
/// Ordering is by the slug's trailing index, numerically — a message with
/// more than nine attachments still reads 1, 2, … 10, not 1, 10, 2.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_by_source_message_id(
    _db: &ApplicationStore,
    project_id: Uuid,
    source_message_id: &str,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    let mut assets=native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.source_message_id=$1->>'message' AND r.deleted_at IS NULL",json!({"project":project_id,"message":source_message_id})).await?;
    assets.sort_by_key(|asset| mail_attachment_index(asset.slug.as_deref()));
    Ok(assets)
}

/// Every asset on a Project, soft-deleted ones included, newest first — what
/// a lawyer reads to find a document to restore.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn for_project_including_deleted(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(
        &pg,
        "AND r.project_id=($1->>'project')::uuid ORDER BY id DESC",
        json!({"project":project_id}),
    )
    .await
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
    db: &ApplicationStore,
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
pub async fn exists_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<bool, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::count(
        &pg,
        "AND r.project_id=($1->>'project')::uuid",
        json!({"project":project_id}),
    )
    .await?
        > 0)
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
    _db: &ApplicationStore,
    project_id: Uuid,
    filename: &str,
) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.filename=$1->>'filename' AND r.deleted_at IS NULL ORDER BY id DESC LIMIT 1",json!({"project":project_id,"filename":filename})).await? .into_iter().next())
}

/// The asset on `project_id` matching `(filename, sha256_hex)` — the
/// re-upload dedup probe. Deliberately narrower than the content hash
/// alone: the same bytes under a different name are a different document.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_filed_copy(
    _db: &ApplicationStore,
    project_id: Uuid,
    filename: &str,
    sha256_hex: &str,
) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.filename=$1->>'filename' AND r.sha256_hex=$1->>'sha' AND r.deleted_at IS NULL ORDER BY id DESC LIMIT 1",json!({"project":project_id,"filename":filename,"sha":sha256_hex})).await? .into_iter().next())
}

/// The oldest live asset on `project_id` holding exactly `sha256_hex`,
/// whatever its name — the probe the inbound-mail filer uses so a file
/// already on the matter is not filed again under a second identity.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn find_live_by_sha256(
    _db: &ApplicationStore,
    project_id: Uuid,
    sha256_hex: &str,
) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.sha256_hex=$1->>'sha' AND r.deleted_at IS NULL ORDER BY id ASC LIMIT 1",json!({"project":project_id,"sha":sha256_hex})).await? .into_iter().next())
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
    _db: &ApplicationStore,
    project_id: Uuid,
    filename: &str,
    sha256_hex: &str,
    visibility: &str,
) -> Result<usize, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    let rows=native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.filename=$1->>'filename' AND r.sha256_hex=$1->>'sha' AND r.visibility<>$1->>'visibility'",json!({"project":project_id,"filename":filename,"sha":sha256_hex,"visibility":visibility})).await?;
    for row in &rows {
        native::patch(&pg, row.id, json!({"visibility":visibility}))
            .await?
            .ok_or(AssetError::WriteReturnedNothing)?;
    }
    Ok(rows.len())
}

/// The newest asset on `project_id` carrying `kind`, or `None`.
///
/// Newest by insertion order (`id` descending, UUIDv7), the same ordering
/// [`current`] uses — not `inserted_at`, so the two cannot disagree.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn latest_of_kind(
    _db: &ApplicationStore,
    project_id: Uuid,
    kind: &str,
) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.kind=$1->>'kind' AND r.deleted_at IS NULL ORDER BY id DESC LIMIT 1",json!({"project":project_id,"kind":kind})).await? .into_iter().next())
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
    _db: &ApplicationStore,
    project_id: Uuid,
    storage_key: &str,
) -> Result<bool, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::count(&pg,"AND (r.storage_key=$1->>'key' OR r.secondary_storage_key=$1->>'key') AND r.project_id IS NOT NULL AND r.project_id<>($1->>'project')::uuid",json!({"project":project_id,"key":storage_key})).await? > 0)
}

/// Whether an unpurged row other than those in `excluding` still points at
/// `storage_key`, on any Project. A retention purge or active-copy cleanup
/// keeps an object any such row needs, including a sibling chain on the same
/// matter that shares the content-addressed bytes.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn storage_key_needed_elsewhere(
    _db: &ApplicationStore,
    storage_key: &str,
    excluding: &[Uuid],
) -> Result<bool, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::count(&pg,"AND (r.storage_key=$1->>'key' OR r.secondary_storage_key=$1->>'key') AND r.purged_at IS NULL AND r.id NOT IN(SELECT jsonb_array_elements_text($1->'excluded')::uuid)",json!({"key":storage_key,"excluded":excluding})).await? > 0)
}

/// Whether an unpurged row on `project_id` other than those in `excluding`
/// holds bytes with this digest.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn digest_needed_elsewhere(
    _db: &ApplicationStore,
    project_id: Uuid,
    sha256_hex: &str,
    excluding: &[Uuid],
) -> Result<bool, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::count(&pg,"AND r.project_id=($1->>'project')::uuid AND r.sha256_hex=$1->>'sha' AND r.purged_at IS NULL AND r.id NOT IN(SELECT jsonb_array_elements_text($1->'excluded')::uuid)",json!({"project":project_id,"sha":sha256_hex,"excluded":excluding})).await? > 0)
}

/// Every soft-deleted row the purge has not removed, oldest first, at most
/// `limit`: the chains the retention lane archives.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn soft_deleted_unpurged(
    _db: &ApplicationStore,
    limit: usize,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(&pg,"AND r.deleted_at IS NOT NULL AND r.purged_at IS NULL ORDER BY id ASC LIMIT ($1->>'limit')::bigint",json!({"limit":i64::try_from(limit).unwrap_or(i64::MAX)})).await
}

/// Every revision of the chain `asset` belongs to, soft-deleted or not.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn chain(db: &ApplicationStore, asset: &Asset) -> Result<Vec<Asset>, AssetError> {
    chain_of(db, asset).await
}

/// How many assets are filed across `project_ids`, in one round trip —
/// the web app's projects-dashboard KPI tile, which needs the number and none of the
/// rows. An empty slice never reaches the engine.
///
/// # Errors
/// [`AssetError::Db`] if the count fails.
pub async fn count_for_projects(
    _db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<usize, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::count(&pg,"AND r.project_id IN(SELECT jsonb_array_elements_text($1->'projects')::uuid) AND r.deleted_at IS NULL",json!({"projects":project_ids})).await
}

/// `(project_id, kind)` for every asset filed across `project_ids`, in one
/// round trip — what the matter-lifecycle fold needs and nothing else, so it
/// reads a narrow projection rather than every [`Asset`] field. An empty
/// slice never reaches the engine.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn kinds_by_projects(
    _db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<(Uuid, Option<String>)>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(sqlx::query_as(
        "SELECT project_id,kind FROM asset WHERE project_id=ANY($1) AND deleted_at IS NULL",
    )
    .bind(project_ids)
    .fetch_all(pg.pool())
    .await?)
}

/// Server-stamped evidence that an onboarding upload names a completed DocuSign envelope.
pub const VERIFIED_ONBOARDING_ENVELOPE_KEY: &str = "verified_onboarding_envelope_id";

pub(crate) fn verified_onboarding_envelope(metadata: Option<&serde_json::Value>) -> Option<Uuid> {
    let metadata = metadata?;
    let id = crate::document_pointers::envelope_id_from_asset_metadata(Some(metadata))?;
    (!id.is_nil()
        && metadata
            .get(VERIFIED_ONBOARDING_ENVELOPE_KEY)?
            .as_str()?
            .parse::<Uuid>()
            .ok()
            == Some(id))
    .then_some(id)
}

/// Every asset, newest first — the `/app/lawyer/assets` transparency listing.
///
/// Unbounded. That is a real
/// cost on a large deployment and the reason this is not a hot path: it is
/// a lawyer directory, not something a matter surface reads.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn list_all(_db: &ApplicationStore) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(&pg, "ORDER BY id DESC", json!({})).await
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
pub async fn delete(_db: &ApplicationStore, id: Uuid) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::delete(&pg, id).await
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
    _db: &ApplicationStore,
    storage_key: &str,
) -> Result<bool, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(native::count(
        &pg,
        "AND r.storage_key=$1->>'key'",
        json!({"key":storage_key}),
    )
    .await?
        > 0)
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
    _db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    bytes: &[u8],
    content_type: &str,
) -> Result<Uuid, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::ingest_content(&pg, storage, bytes, content_type).await
}

/// Fetch an asset's bytes from storage by asset id.
///
/// # Errors
/// [`AssetError`] when the row is missing or storage fails.
pub async fn fetch(
    db: &ApplicationStore,
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
/// - *Latest visible to you.* The client keeps seeing v2 while lawyers
///   iterate on v3. A revision filed through [`file_revision`] sets the
///   chain beneath it to `internal`, so v2 stays in front of the client
///   only when a lawyer shares it again.
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
    _db: &ApplicationStore,
    project_id: Uuid,
    slug: &str,
    lens: Lens,
) -> Result<Option<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    let suffix=match lens { Lens::Lawyer=>"AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' AND r.deleted_at IS NULL ORDER BY id DESC LIMIT 1", Lens::Client=>"AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' AND r.deleted_at IS NULL AND r.published_at IS NOT NULL AND r.visibility=$1->>'visible' ORDER BY id DESC LIMIT 1" };
    Ok(native::rows(
        &pg,
        suffix,
        json!({"project":project_id,"slug":slug,"visible":crate::documents::visibility::CLIENT}),
    )
    .await?
    .into_iter()
    .next())
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
    _db: &ApplicationStore,
    project_id: Uuid,
    slug: &str,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(
        &pg,
        "AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' ORDER BY id DESC",
        json!({"project":project_id,"slug":slug}),
    )
    .await
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
    _db: &ApplicationStore,
    project_id: Uuid,
    slugs: &[String],
) -> Result<Vec<Asset>, AssetError> {
    if slugs.is_empty() {
        return Ok(Vec::new());
    }
    let pg = matter::authority_for(TABLE).await?;
    native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.slug IN(SELECT jsonb_array_elements_text($1->'slugs')) ORDER BY slug ASC,id DESC",json!({"project":project_id,"slugs":slugs})).await
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
    db: &ApplicationStore,
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
    patch_postgres(asset_id, json!({"visibility":visibility})).await?;

    Ok(Some(true))
}

/// Why [`set_folder`] was refused (LAW-241).
#[derive(Debug, thiserror::Error)]
pub enum SetFolderError {
    /// The folder name is not one of the Firm's matter folders, spelled
    /// exactly. Fails closed: an unknown value is an error, never a
    /// silently-defaulted placement.
    #[error(transparent)]
    UnknownFolder(#[from] crate::document_pointers::UnknownFolderError),
    #[error(transparent)]
    Asset(#[from] AssetError),
}

/// Set (or clear) the matter folder on one asset that belongs to
/// `project_id` (LAW-241): the pointer's `folder:` key, persisted so the
/// live matter files the document where the pointer says. `None` is an
/// explicit clear back to the kind's default placement, not an omission.
///
/// `Ok(None)` when the asset is absent or belongs to another Project;
/// `Ok(Some(false))` when it already sits in `folder`; `Ok(Some(true))`
/// after writing, mirroring the change to PostgreSQL.
///
/// # Errors
/// [`SetFolderError`] — a rejected folder name or a database failure.
pub async fn set_folder(
    db: &ApplicationStore,
    project_id: Uuid,
    asset_id: Uuid,
    folder: Option<&str>,
) -> Result<Option<bool>, SetFolderError> {
    let folder = folder
        .map(crate::document_pointers::MatterFolder::parse_checked)
        .transpose()?;
    let current = find_by_id(db, asset_id).await?;
    let Some(asset) = current.filter(|asset| asset.project_id == Some(project_id)) else {
        return Ok(None);
    };
    if asset.folder.as_deref() == folder.map(crate::document_pointers::MatterFolder::as_str) {
        return Ok(Some(false));
    }
    patch_postgres(
        asset_id,
        json!({"folder":folder.map(crate::document_pointers::MatterFolder::as_str)}),
    )
    .await?;

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
/// chain, or the row alone when it has no slug. A blank slug is not an
/// identity: rows that share it stay independent.
async fn chain_of(db: &ApplicationStore, asset: &Asset) -> Result<Vec<Asset>, AssetError> {
    let Some(project_id) = asset.project_id else {
        return Ok(vec![asset.clone()]);
    };
    let Some(slug) = asset.slug.as_deref().filter(|slug| !slug.is_empty()) else {
        return Ok(vec![asset.clone()]);
    };
    let mut chain = revisions(db, project_id, slug).await?;
    if !chain.iter().any(|row| row.id == asset.id) {
        chain.push(asset.clone());
    }
    Ok(chain)
}

struct StampTarget {
    project_id: Option<Uuid>,
    slug: Option<String>,
    kind: Option<String>,
    deleted_at: Option<DateTime<Utc>>,
    purge_after: Option<DateTime<Utc>>,
}

struct StampMember {
    id: Uuid,
    deleted_at: Option<DateTime<Utc>>,
    purge_after: Option<DateTime<Utc>>,
}

async fn read_stamp_target(
    _db: &ApplicationStore,
    asset_id: Uuid,
) -> Result<Option<StampTarget>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    let row = native::rows(&pg, "AND r.id=($1->>'id')::uuid", json!({"id":asset_id}))
        .await?
        .into_iter()
        .next();
    Ok(row.map(|asset| StampTarget {
        project_id: asset.project_id,
        slug: asset.slug,
        kind: asset.kind,
        deleted_at: asset.deleted_at,
        purge_after: asset.purge_after,
    }))
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
    db: &ApplicationStore,
    project_id: Uuid,
    asset_id: Uuid,
    deleted_by: Uuid,
    commit_sha: Option<&str>,
    now: DateTime<Utc>,
) -> Result<SoftDeletion, SoftDeleteError> {
    let Some(target) = read_stamp_target(db, asset_id).await? else {
        return Err(SoftDeleteError::NotOnProject);
    };
    if target.project_id != Some(project_id) {
        return Err(SoftDeleteError::NotOnProject);
    }
    if target.kind.as_deref() == Some(rules::kind::Kind::ClosedRepository.as_str()) {
        return Err(SoftDeleteError::ClosedRepository);
    }
    let slug = target
        .slug
        .as_deref()
        .filter(|slug| !slug.is_empty())
        .map(str::to_string);
    let members = stamp_members(db, project_id, asset_id, &target, slug.as_deref()).await?;
    if let Some((deleted_at, purge_after)) = members
        .iter()
        .all(|row| row.deleted_at.is_some())
        .then(|| {
            members
                .iter()
                .find_map(|row| row.deleted_at.zip(row.purge_after))
        })
        .flatten()
    {
        return Ok(SoftDeletion {
            slug,
            revisions: members.len(),
            deleted_at,
            purge_after,
            changed: false,
        });
    }
    let purge_after = now + chrono::Duration::days(RETENTION_DAYS);
    for row in members.iter().filter(|row| row.deleted_at.is_none()) {
        stamp_row(db, row.id, deleted_by, commit_sha, now, purge_after).await?;
    }
    let _stamped: Vec<Uuid> = members
        .iter()
        .filter(|row| row.deleted_at.is_none())
        .map(|row| row.id)
        .collect();
    Ok(SoftDeletion {
        slug,
        revisions: members.len(),
        deleted_at: now,
        purge_after,
        changed: true,
    })
}

async fn stamp_members(
    db: &ApplicationStore,
    project_id: Uuid,
    asset_id: Uuid,
    target: &StampTarget,
    slug: Option<&str>,
) -> Result<Vec<StampMember>, AssetError> {
    let Some(slug) = slug else {
        return Ok(vec![member_from_target(asset_id, target)]);
    };
    let chain = revisions(db, project_id, slug).await?;
    let mut members: Vec<StampMember> = chain
        .into_iter()
        .map(|row| StampMember {
            id: row.id,
            deleted_at: row.deleted_at,
            purge_after: row.purge_after,
        })
        .collect();
    if !members.iter().any(|row| row.id == asset_id) {
        members.push(member_from_target(asset_id, target));
    }
    Ok(members)
}

fn member_from_target(asset_id: Uuid, target: &StampTarget) -> StampMember {
    StampMember {
        id: asset_id,
        deleted_at: target.deleted_at,
        purge_after: target.purge_after,
    }
}

async fn stamp_row(
    _db: &ApplicationStore,
    asset_id: Uuid,
    deleted_by: Uuid,
    commit_sha: Option<&str>,
    now: DateTime<Utc>,
    purge_after: DateTime<Utc>,
) -> Result<(), AssetError> {
    patch_postgres(asset_id,json!({"deleted_at":now,"purge_after":purge_after,"deleted_by_person_id":deleted_by,"deleted_commit_sha":commit_sha})).await?;

    Ok(())
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
    db: &ApplicationStore,
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
        patch_postgres(row.id,json!({"deleted_at":null,"purge_after":null,"deleted_by_person_id":null,"deleted_commit_sha":null})).await?;
        changed = true;
    }
    let _cleared: Vec<Uuid> = chain
        .iter()
        .filter(|row| row.deleted_at.is_some())
        .map(|row| row.id)
        .collect();
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
    db: &ApplicationStore,
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
        patch_postgres(row.id, json!({"legal_hold_at":held.then_some(now)})).await?;
    }
    let _held: Vec<Uuid> = chain.iter().map(|row| row.id).collect();
    Ok(Some(chain.len()))
}

/// Every soft-deleted row whose retention window has closed and whose bytes
/// the purge has not yet expunged, oldest first. The caller still decides
/// which of them a Legal Hold or an unarchived matter protects.
///
/// # Errors
/// [`AssetError::Db`] if the lookup fails.
pub async fn due_for_purge(
    _db: &ApplicationStore,
    now: DateTime<Utc>,
) -> Result<Vec<Asset>, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::rows(&pg,"AND r.deleted_at IS NOT NULL AND r.purged_at IS NULL AND r.purge_after<=($1->>'now')::timestamptz ORDER BY id ASC",json!({"now":now})).await
}

/// Record that the governed expunge removed the bytes of every row in `ids`.
///
/// # Errors
/// [`AssetError`] on a database failure.
pub async fn mark_purged(
    _db: &ApplicationStore,
    ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<(), AssetError> {
    for id in ids {
        patch_postgres(*id, json!({"purged_at":now})).await?;
    }
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
pub async fn restore_archived(
    _db: &ApplicationStore,
    asset: &Asset,
) -> Result<RowRestore, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    native::restore(&pg, asset).await
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
    db: &ApplicationStore,
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
    let mut changes = json!({"slug":slug});
    if let Some(kind) = &kind {
        changes["kind"] = json!(kind);
    }
    patch_postgres(asset_id, changes).await?;

    Ok(find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?)
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
    db: &ApplicationStore,
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
        patch_postgres(id, json!({"kind":kind})).await?;
    }
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
    db: &ApplicationStore,
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
        patch_postgres(id, json!({"kind":kind})).await?;
    }
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
    db: &ApplicationStore,
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
        patch_postgres(id, json!({"slug":new_slug})).await?;
    }
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
    db: &ApplicationStore,
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
    let code = repair_project_code(db, project_id).await?;
    let canonical = format!(
        "{}/{}",
        cloud::workspace::documents_prefix(&code),
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
        patch_postgres(asset_id, json!({"storage_key":canonical})).await?;
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

async fn repair_project_code(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<String, RepairStorageError> {
    let authority = matter::authority_for(TABLE)
        .await
        .map_err(AssetError::from)?;
    let missing = crate::projects::ProjectStoreError::NoSuchProject(project_id);
    let code = {
        let pg = authority;
        native::project_code(&pg, project_id)
            .await?
            .ok_or(missing)?
    };
    Ok(code)
}

/// Why a [`file_revision`] call wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum RevisionError {
    #[error("recursive Drive document metadata must be a JSON object")]
    InvalidTreeMetadata,
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
    /// A conditional append's expected head no longer matches the live
    /// chain (LAW-284). Carries the live head's triple — `None` when the
    /// chain has no live head at all — so the caller can restart against
    /// the current head without a second read.
    #[error("document `{slug}` moved past the expected head")]
    HeadConflict {
        slug: String,
        current: Option<ExpectedHead>,
    },
    #[error(transparent)]
    Ingest(#[from] crate::documents::IngestError),
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

/// The chain head a caller read before uploading — the compare-and-swap
/// token of a conditional append (LAW-284). The same asset id / version /
/// digest triple the transcript door's `derived_from` validation already
/// checks, carried on the upload request and re-validated at the insert.
///
/// The version is the head's one-based position in its chain — the same
/// numbering the revisions door reports, so a client can copy it straight
/// off the read it already did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedHead {
    pub asset_id: Uuid,
    pub version: u32,
    pub sha256: String,
}

impl std::fmt::Display for ExpectedHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "asset {} version {} sha256 {}",
            self.asset_id, self.version, self.sha256
        )
    }
}

/// The expected-head triple of a chain read: the live head's id, its
/// one-based version (the chain's length — the same numbering the
/// revisions door reports), and its byte digest.
pub(crate) fn head_triple(head: &Asset, chain_len: usize) -> ExpectedHead {
    ExpectedHead {
        asset_id: head.id,
        version: u32::try_from(chain_len).unwrap_or(u32::MAX),
        sha256: head.sha256_hex.clone(),
    }
}

/// File `bytes` as the next revision of the document `(project_id,
/// identity.slug)` — the **asset lane's write boundary**.
///
/// Four rules live here and nowhere else, so no call site reimplements
/// one and drifts:
///
/// 1. **The lane is closed.** `args.kind` must be
///    [`rules::kind::Lane::Asset`]-valid. A teaching page or a dashboard
///    skeleton is never a byte artifact filed on a matter.
/// 2. **Kind is immutable across a chain.** A slug that already holds a
///    `retainer` does not accept an `agreement` revision — that is a
///    different document and belongs under its own slug.
/// 3. **Identical bytes and verified execution evidence are a no-op**, scoped to this one slug on this
///    one project. Never a global "we already have these bytes" probe:
///    that is an existence oracle, and legal documents are boilerplate
///    plus a few low-entropy fields — exactly the shape Harnik et al.
///    (IEEE S&P 2010) showed lets an outsider confirm a document's
///    contents by uploading a guess. The same bytes under a *different*
///    slug are a genuinely different document and do create a revision.
/// 4. **A superseded revision is internal.** Filing a new revision sets
///    every earlier one in the chain to `internal` ([`demote_superseded`]),
///    so a stale copy never stays client-visible by default.
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, or on a storage or
/// database failure.
pub async fn file_revision(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(db, storage, args, identity, None, None, bytes).await
}

/// [`file_revision`] with a compare-and-swap (LAW-284): when
/// `expected_head` is `Some`, the chain's live head is re-read inside the
/// insert path and validated against the caller's triple before anything
/// is written — a head that moved is [`RevisionError::HeadConflict`]
/// carrying the live triple, never a silently forked chain. Uploads whose
/// bytes the live head already holds resolve to
/// [`Filed::Unchanged`] regardless of the triple — the cross-run
/// idempotency a retried upload after an ambiguous timeout relies on.
/// `None` files exactly as [`file_revision`] always has (the old-CLI and
/// internal-lane path: no CAS, no late dedupe re-check).
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, the expected head no
/// longer matches, or on a storage or database failure.
pub async fn file_revision_with_expected_head(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    expected_head: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(db, storage, args, identity, None, expected_head, bytes).await
}

/// [`file_revision`], additionally stamping [`crate::documents::MailProvenance`]
/// onto a newly-inserted revision (ENG-517). The one caller is `navigator
/// site mail file`'s server-side handler.
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, or on a storage or
/// database failure.
pub async fn file_revision_with_provenance(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: &crate::documents::MailProvenance<'_>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(db, storage, args, identity, Some(provenance), None, bytes).await
}

/// [`file_revision_with_provenance`] with a compare-and-swap (LAW-284):
/// the mail-lane upload path (`navigator mail parse`) files a `.eml`
/// source document or one of its attachments with
/// [`crate::documents::MailProvenance`] stamped, while honoring the
/// caller's `expected_head` exactly as [`file_revision_with_expected_head`]
/// does. The one caller is the CLI's document-lane upload of mail bytes.
///
/// # Errors
/// [`RevisionError`] when a rule rejects the write, the expected head no
/// longer matches, or on a storage or database failure.
pub async fn file_revision_with_provenance_and_expected_head(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: &crate::documents::MailProvenance<'_>,
    expected_head: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    file_revision_inner(
        db,
        storage,
        args,
        identity,
        Some(provenance),
        expected_head,
        bytes,
    )
    .await
}

pub(crate) fn preserve_drive_metadata(
    identity: &mut crate::documents::DocumentIdentity<'_>,
    head: &Asset,
) -> Result<(), RevisionError> {
    if head.drive_checkout_path().is_none() {
        return Ok(());
    }
    let target = identity
        .metadata
        .get_or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(RevisionError::InvalidTreeMetadata)?;
    if let Some(source) = head
        .metadata
        .as_ref()
        .and_then(serde_json::Value::as_object)
    {
        for (key, value) in source.iter().filter(|(key, _)| key.starts_with("drive_")) {
            target.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    Ok(())
}

async fn file_revision_inner(
    _db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: &crate::documents::IngestArgs<'_>,
    identity: &crate::documents::DocumentIdentity<'_>,
    provenance: Option<&crate::documents::MailProvenance<'_>>,
    expected_head: Option<&ExpectedHead>,
    bytes: &[u8],
) -> Result<Filed, RevisionError> {
    let pg = matter::authority_for(TABLE)
        .await
        .map_err(AssetError::from)?;
    native::file_revision(
        &pg,
        storage,
        args,
        identity,
        provenance,
        expected_head,
        bytes,
    )
    .await
}

/// Refresh a verified import's source receipt and placement without forking its identity.
pub(crate) async fn refresh_drive_import(
    db: &ApplicationStore,
    project_id: Uuid,
    asset_id: Uuid,
    file_id: &str,
    receipt: &serde_json::Value,
    filename: &str,
) -> Result<(), AssetError> {
    let head = find_by_id(db, asset_id)
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    if head.project_id != Some(project_id)
        || head
            .metadata
            .as_ref()
            .and_then(|m| m.get("drive_file_id"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id != file_id)
    {
        return Err(AssetError::WriteReturnedNothing);
    }
    let mut metadata = head
        .metadata
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let target = metadata
        .as_object_mut()
        .ok_or(AssetError::WriteReturnedNothing)?;
    for (key, value) in receipt
        .as_object()
        .ok_or(AssetError::WriteReturnedNothing)?
    {
        target.insert(key.clone(), value.clone());
    }
    if head.metadata.as_ref() == Some(&metadata) && head.filename.as_deref() == Some(filename) {
        return Ok(());
    }
    patch_postgres(asset_id, json!({"metadata":metadata,"filename":filename})).await?;

    Ok(())
}

/// Set every revision of `(project_id, slug)` except `current` to
/// `internal`, returning how many changed.
///
/// A new revision supersedes the chain beneath it, and a superseded copy
/// stays lawyer-only unless a lawyer deliberately shares it again through
/// [`set_visibility`]. The `visibility != $internal` predicate touches only
/// the rows still shared, so a chain already internal writes nothing.
///
/// # Errors
/// [`AssetError::Db`] if the update fails.
pub async fn demote_superseded(
    _db: &ApplicationStore,
    project_id: Uuid,
    slug: &str,
    current: Uuid,
) -> Result<usize, AssetError> {
    let pg = matter::authority_for(TABLE).await?;
    let rows=native::rows(&pg,"AND r.project_id=($1->>'project')::uuid AND r.slug=$1->>'slug' AND r.id<>($1->>'current')::uuid AND r.visibility<>$1->>'internal'",json!({"project":project_id,"slug":slug,"current":current,"internal":crate::documents::visibility::INTERNAL})).await?;
    for row in &rows {
        native::patch(
            &pg,
            row.id,
            json!({"visibility":crate::documents::visibility::INTERNAL}),
        )
        .await?
        .ok_or(AssetError::WriteReturnedNothing)?;
    }
    Ok(rows.len())
}

#[cfg(test)]
mod tests {
    use super::{revisions_for_slugs, sha256_hex};
    use crate::application_store::ApplicationStore;

    use uuid::Uuid;

    #[tokio::test]
    async fn an_empty_revision_slug_batch_does_not_touch_the_database() {
        let db = ApplicationStore::uninitialized();
        assert!(revisions_for_slugs(&db, Uuid::now_v7(), &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn sha256_hex_matches_the_known_digest() {
        assert_eq!(
            sha256_hex(b"hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }
}
