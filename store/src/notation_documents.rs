//! `notation_documents` — the durable, append-only Notation Markdown
//! document-source model for one Notation (ENG-578).
//!
//! # Why this is not `templates` or `notation_clauses`
//!
//! An imported Word document is matter work product, not a reusable
//! Git-backed [`crate::templates`] blueprint, and it is a full document, not
//! one custom paragraph a [`crate::notation_clauses`] row splices into
//! `{{custom_clauses}}`. This module owns a third shape: one document
//! identity per Notation (tied to the immutable original Word
//! [`crate::assets`] row it was parsed from), and an append-only chain of
//! immutable Markdown-source versions hanging off that identity. A version
//! is never rewritten — a re-parse or an attorney edit always appends a
//! child.
//!
//! PostgreSQL owns these identities and versions after the Notation family's
//! verified scoped enrollment. Compatibility reads and writes remain available
//! only without configured PostgreSQL; provisional imports fail closed.
//! See `docs/notation.md` for the document-source vocabulary.
//!
//! # Content never enters Git, logs, or the workflow journal
//!
//! `markdown_asset_id` and `anchor_manifest_asset_id` are internal,
//! content-addressed [`crate::assets`] rows (`visibility::INTERNAL`, never
//! `client`) written through [`crate::assets::ingest_content`]. The
//! `notation_event` rows this module journals ([`MACHINE_DOCUMENT`]) carry
//! only identifiers — asset ids, version ids, parser/schema versions — never
//! Markdown bytes or the anchor manifest's contents.
//!
//! # Idempotency and conflict, not a lock
//!
//! [`import_root`] is idempotent: re-parsing the same original asset into
//! byte-identical Markdown with the same parser/schema version is a no-op
//! that returns the existing root version rather than forking the chain.
//! [`append_edit`] requires the caller's `expected_parent_version_id` to
//! still be the document's current version; a caller racing against another
//! writer gets [`NotationDocumentError::Conflict`] rather than silently
//! overwriting or forking. A conflict caught before any write refuses
//! cleanly; a conflict caught only by the compare-and-swap after the version
//! row was already written (a true concurrent race) leaves that row in
//! place, orphaned rather than current — a version is append-only and
//! nothing here ever deletes one.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use cloud::StorageService;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;

pub(crate) const DOCUMENT_TABLE: &str = "notation_document";
pub(crate) const VERSION_TABLE: &str = "notation_document_version";

/// The version that came straight from parsing the original Word package.
pub const STATUS_IMPORTED: &str = "imported";
/// A version an attorney produced by editing a parent version (ENG-579).
pub const STATUS_EDITED: &str = "edited";

/// `notation_events.machine_kind` for a document-version write. A distinct
/// kind so these rows never participate in the workflow / questionnaire
/// state-projection reads.
pub const MACHINE_DOCUMENT: &str = "document";

/// One Notation's document-source identity.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`DocumentRow`] is the seam that turns it into (and back out of) what the
/// SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationDocument {
    pub id: Uuid,
    pub notation_id: Uuid,
    /// The immutable original Word package this chain was parsed from.
    /// Fixed at creation — never rewritten.
    pub original_asset_id: Uuid,
    /// The current-version projection. `None` only before the first
    /// [`import_root`] call succeeds.
    pub current_version_id: Option<Uuid>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One immutable Markdown-source version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationDocumentVersion {
    pub id: Uuid,
    pub document_id: Uuid,
    /// `None` only for the root version created by [`import_root`].
    pub parent_version_id: Option<Uuid>,
    /// The internal, content-addressed asset holding this version's
    /// Notation Markdown bytes.
    pub markdown_asset_id: Uuid,
    /// The internal, content-addressed asset holding this version's
    /// block/anchor manifest (JSON) — see the module doc's anchor-durability
    /// note.
    pub anchor_manifest_asset_id: Uuid,
    pub parser_version: String,
    /// The canonical outline schema version the Markdown was projected
    /// against.
    pub schema_version: i64,
    pub authored_by_person_id: Uuid,
    /// [`STATUS_IMPORTED`] or [`STATUS_EDITED`].
    pub status: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Why a `notation_documents` command refused.
#[derive(Debug, thiserror::Error)]
pub enum NotationDocumentError {
    #[error("PostgreSQL notation document operation failed")]
    Postgres(#[from] sqlx::Error),
    #[error(transparent)]
    Authority(#[from] crate::postgres::matter::MatterAuthorityError),
    #[error("notation: {0}")]
    Notation(#[from] crate::notations::NotationError),
    #[error("asset: {0}")]
    Asset(#[from] crate::assets::AssetError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a notation document returned no usable row")]
    WriteReturnedNothing,
    /// `notation_id` resolves to a different Project than the caller
    /// asserted — an unrelated participant's project id can never reach a
    /// document it does not own.
    #[error("notation {notation_id} does not belong to project {project_id}")]
    ProjectMismatch { notation_id: Uuid, project_id: Uuid },
    /// A second [`import_root`] named a different original asset than the
    /// document's existing baseline. A changed baseline is a different
    /// document; it must not silently rebase the chain.
    #[error(
        "notation {notation_id} already has an original asset {existing}; {attempted} is a \
         different baseline"
    )]
    OriginalAssetMismatch {
        notation_id: Uuid,
        existing: Uuid,
        attempted: Uuid,
    },
    /// The caller's `expected_parent_version_id` (or, for [`import_root`],
    /// the absence of one) no longer matches the document's current
    /// version. Nothing was written to the current-version pointer; the
    /// version row this call may have appended, if any, is preserved but
    /// orphaned rather than lost.
    #[error("expected parent {expected:?} does not match current version {actual:?}")]
    Conflict {
        expected: Option<Uuid>,
        actual: Option<Uuid>,
    },
    #[error("notation document for notation {0} not found")]
    DocumentNotFound(Uuid),
    #[error("notation document version {0} not found")]
    VersionNotFound(Uuid),
    #[error("notation document version body is not valid UTF-8")]
    NotUtf8,
    #[error("notation event: {0}")]
    NotationEvent(#[from] crate::notation_events::NotationEventError),
}

/// Resolve `notation_id`, asserting it belongs to `project_id`.
///
/// This is the module's one authorization primitive: every public read and
/// write threads a caller-asserted `project_id` through this check, so an
/// unrelated participant's project id can never reach another Project's
/// document or version — the same shape [`crate::assets::set_visibility`]
/// uses for its Project scope.
async fn resolve_scoped_notation(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<crate::notations::Notation, NotationDocumentError> {
    let notation = crate::notations::find_by_id(db, notation_id)
        .await?
        .ok_or(crate::notations::NotationError::NotFound(notation_id))?;
    if notation.project_id != project_id {
        return Err(NotationDocumentError::ProjectMismatch {
            notation_id,
            project_id,
        });
    }
    Ok(notation)
}

async fn find_document_by_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<NotationDocument>, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(DOCUMENT_TABLE).await?;
    Ok(crate::postgres::matter::find(
        &pg,
        DOCUMENT_TABLE,
        serde_json::json!({"notation_id":notation_id}),
    )
    .await?)
}

/// The document identity for a Notation, scoped to `project_id`.
///
/// Returns `None` when the Notation has never had a document imported, and
/// [`NotationDocumentError::ProjectMismatch`] when `notation_id` belongs to
/// a different Project.
///
/// # Errors
/// Propagates any database error, or a project-scope mismatch.
pub async fn find_document(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<Option<NotationDocument>, NotationDocumentError> {
    resolve_scoped_notation(db, project_id, notation_id).await?;
    find_document_by_notation(db, notation_id).await
}

async fn find_version_by_id_unscoped(
    _db: &ApplicationStore,
    version_id: Uuid,
) -> Result<Option<NotationDocumentVersion>, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(VERSION_TABLE).await?;
    Ok(
        crate::postgres::matter::find(&pg, VERSION_TABLE, serde_json::json!({"id":version_id}))
            .await?,
    )
}

/// The current version of a Notation's document, scoped to `project_id`.
///
/// Returns `None` when no document exists yet, or none has been imported.
///
/// # Errors
/// Propagates any database error, or a project-scope mismatch.
pub async fn current_version(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<Option<NotationDocumentVersion>, NotationDocumentError> {
    let Some(document) = find_document(db, project_id, notation_id).await? else {
        return Ok(None);
    };
    let Some(current_id) = document.current_version_id else {
        return Ok(None);
    };
    find_version_by_id_unscoped(db, current_id).await
}

/// Every version of a Notation's document, newest first — both the current
/// version and every version it superseded remain readable and
/// attributable.
///
/// # Errors
/// Propagates any database error, or a project-scope mismatch.
pub async fn history(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
) -> Result<Vec<NotationDocumentVersion>, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(VERSION_TABLE).await?;
    let Some(document) = find_document(db, project_id, notation_id).await? else {
        return Ok(Vec::new());
    };
    Ok(crate::postgres::matter::select(
        &pg,
        VERSION_TABLE,
        serde_json::json!({"document_id":document.id}),
        &["inserted_at DESC", "id DESC"],
    )
    .await?)
}

/// Every version of a document identity, oldest first, whatever its
/// Notation's Project. The recovery archive reads whole chains this way.
///
/// # Errors
/// Propagates any database error.
pub async fn versions_of(
    _db: &ApplicationStore,
    document_id: Uuid,
) -> Result<Vec<NotationDocumentVersion>, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(VERSION_TABLE).await?;
    Ok(crate::postgres::matter::select(
        &pg,
        VERSION_TABLE,
        serde_json::json!({"document_id":document_id}),
        &["inserted_at", "id"],
    )
    .await?)
}

/// The document identity a Notation owns, if one was ever imported.
///
/// # Errors
/// Propagates any database error.
pub async fn document_for_notation(
    db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<NotationDocument>, NotationDocumentError> {
    find_document_by_notation(db, notation_id).await
}

/// Write one archived document identity back under its original id. An
/// identical row is left alone; a different one is a conflict.
///
/// # Errors
/// Propagates any database error.
pub async fn restore_archived_document(
    _db: &ApplicationStore,
    document: &NotationDocument,
) -> Result<crate::assets::RowRestore, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(DOCUMENT_TABLE).await?;
    crate::postgres::notation_documents::restore_document(&pg, document).await
}

/// Write one archived Markdown-source version back under its original id. An
/// identical row is left alone; a different one is a conflict.
///
/// # Errors
/// Propagates any database error.
pub async fn restore_archived_version(
    _db: &ApplicationStore,
    version: &NotationDocumentVersion,
) -> Result<crate::assets::RowRestore, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(VERSION_TABLE).await?;
    crate::postgres::notation_documents::restore_version(&pg, version).await
}

/// One specific version, scoped to `project_id` through its document's
/// Notation.
///
/// # Errors
/// Propagates any database error, or a project-scope mismatch.
pub async fn find_version(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    version_id: Uuid,
) -> Result<Option<NotationDocumentVersion>, NotationDocumentError> {
    let Some(document) = find_document(db, project_id, notation_id).await? else {
        return Ok(None);
    };
    let Some(version) = find_version_by_id_unscoped(db, version_id).await? else {
        return Ok(None);
    };
    Ok((version.document_id == document.id).then_some(version))
}

/// Read a version's Notation Markdown bytes back from object storage as
/// text.
///
/// # Errors
/// [`NotationDocumentError::Asset`] on a storage or database failure, or
/// [`NotationDocumentError::NotUtf8`] if the stored bytes are not UTF-8.
pub async fn markdown_of(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    version: &NotationDocumentVersion,
) -> Result<String, NotationDocumentError> {
    let bytes = crate::assets::fetch(db, storage, version.markdown_asset_id).await?;
    String::from_utf8(bytes).map_err(|_| NotationDocumentError::NotUtf8)
}

/// Read a version's block/anchor manifest back from object storage as raw
/// bytes (JSON). Left untyped here deliberately: this store module has no
/// dependency on `word`, so the manifest's shape is the producing caller's
/// contract, not this module's.
///
/// # Errors
/// [`NotationDocumentError::Asset`] on a storage or database failure.
pub async fn anchor_manifest_of(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    version: &NotationDocumentVersion,
) -> Result<Vec<u8>, NotationDocumentError> {
    Ok(crate::assets::fetch(db, storage, version.anchor_manifest_asset_id).await?)
}

/// Outcome of [`import_root`].
#[derive(Debug)]
pub enum Saved {
    /// Re-importing identical bytes at the same parser/schema version was a
    /// no-op; this is the existing root version.
    Unchanged(NotationDocumentVersion),
    /// A new document identity, a new root version, or both were written.
    Written(NotationDocumentVersion),
}

impl Saved {
    /// The version, either way.
    #[must_use]
    pub fn into_model(self) -> NotationDocumentVersion {
        match self {
            Saved::Unchanged(v) | Saved::Written(v) => v,
        }
    }

    /// Whether this call wrote a new version row.
    #[must_use]
    pub fn was_written(&self) -> bool {
        matches!(self, Saved::Written(_))
    }
}

/// What [`import_root`] needs.
pub struct ImportRoot<'a> {
    pub project_id: Uuid,
    pub notation_id: Uuid,
    /// The immutable original Word package this import came from.
    pub original_asset_id: Uuid,
    pub markdown: &'a [u8],
    pub anchor_manifest: &'a [u8],
    pub parser_version: &'a str,
    pub schema_version: i64,
    pub authored_by_person_id: Uuid,
    /// RFC 3339 / ISO 8601, threaded to the journaled `notation_event`.
    pub recorded_at: &'a str,
}

/// Create (or reuse) a Notation's document identity and write its root
/// Markdown-source version from a freshly parsed Word package.
///
/// Idempotent: calling this again with the same `original_asset_id` and
/// byte-identical `markdown`/`anchor_manifest` at the same
/// `parser_version`/`schema_version` is a no-op that returns
/// [`Saved::Unchanged`] rather than forking the chain — the same "identical
/// bytes are a no-op" rule [`crate::assets::file_revision`] applies to
/// documents.
///
/// # Errors
/// [`NotationDocumentError::ProjectMismatch`] if `notation_id` is not in
/// `project_id`; [`NotationDocumentError::OriginalAssetMismatch`] if the
/// document already names a different baseline;
/// [`NotationDocumentError::Conflict`] if the document already has edits on
/// top of its root (re-importing cannot rebase a chain that has moved); or
/// a database/asset error.
pub async fn import_root(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: ImportRoot<'_>,
) -> Result<Saved, NotationDocumentError> {
    resolve_scoped_notation(db, args.project_id, args.notation_id).await?;

    let markdown_asset_id =
        crate::assets::ingest_content(db, storage, args.markdown, "text/markdown").await?;
    let anchor_manifest_asset_id =
        crate::assets::ingest_content(db, storage, args.anchor_manifest, "application/json")
            .await?;

    let document = match find_document_by_notation(db, args.notation_id).await? {
        Some(existing) => {
            if existing.original_asset_id != args.original_asset_id {
                return Err(NotationDocumentError::OriginalAssetMismatch {
                    notation_id: args.notation_id,
                    existing: existing.original_asset_id,
                    attempted: args.original_asset_id,
                });
            }
            existing
        }
        None => create_document(db, args.notation_id, args.original_asset_id).await?,
    };

    if let Some(current_id) = document.current_version_id {
        let current = find_version_by_id_unscoped(db, current_id)
            .await?
            .ok_or(NotationDocumentError::DocumentNotFound(document.id))?;
        let unchanged = current.parent_version_id.is_none()
            && current.markdown_asset_id == markdown_asset_id
            && current.anchor_manifest_asset_id == anchor_manifest_asset_id
            && current.parser_version == args.parser_version
            && current.schema_version == args.schema_version;
        if unchanged {
            return Ok(Saved::Unchanged(current));
        }
        return Err(NotationDocumentError::Conflict {
            expected: None,
            actual: Some(current_id),
        });
    }

    let version = append_version(
        db,
        &document,
        None,
        markdown_asset_id,
        anchor_manifest_asset_id,
        args.parser_version,
        args.schema_version,
        args.authored_by_person_id,
        STATUS_IMPORTED,
    )
    .await?;

    journal(
        db,
        args.notation_id,
        args.authored_by_person_id,
        &document,
        &version,
        args.recorded_at,
        "imported",
    )
    .await?;

    Ok(Saved::Written(version))
}

/// What [`append_edit`] needs.
pub struct AppendEdit<'a> {
    pub project_id: Uuid,
    pub notation_id: Uuid,
    /// The version this edit was made against. Must still be the
    /// document's current version, or the call refuses with
    /// [`NotationDocumentError::Conflict`].
    pub expected_parent_version_id: Uuid,
    pub markdown: &'a [u8],
    pub anchor_manifest: &'a [u8],
    pub parser_version: &'a str,
    pub schema_version: i64,
    pub authored_by_person_id: Uuid,
    /// RFC 3339 / ISO 8601, threaded to the journaled `notation_event`.
    pub recorded_at: &'a str,
}

/// Append a child version on top of `expected_parent_version_id`.
///
/// # Errors
/// [`NotationDocumentError::ProjectMismatch`] if `notation_id` is not in
/// `project_id`; [`NotationDocumentError::DocumentNotFound`] if the
/// Notation has no document yet; [`NotationDocumentError::Conflict`] if
/// another writer already advanced the document past
/// `expected_parent_version_id`; or a database/asset error.
pub async fn append_edit(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    args: AppendEdit<'_>,
) -> Result<NotationDocumentVersion, NotationDocumentError> {
    resolve_scoped_notation(db, args.project_id, args.notation_id).await?;

    let document = find_document_by_notation(db, args.notation_id)
        .await?
        .ok_or(NotationDocumentError::DocumentNotFound(args.notation_id))?;

    if document.current_version_id != Some(args.expected_parent_version_id) {
        return Err(NotationDocumentError::Conflict {
            expected: Some(args.expected_parent_version_id),
            actual: document.current_version_id,
        });
    }

    let markdown_asset_id =
        crate::assets::ingest_content(db, storage, args.markdown, "text/markdown").await?;
    let anchor_manifest_asset_id =
        crate::assets::ingest_content(db, storage, args.anchor_manifest, "application/json")
            .await?;

    let version = append_version(
        db,
        &document,
        Some(args.expected_parent_version_id),
        markdown_asset_id,
        anchor_manifest_asset_id,
        args.parser_version,
        args.schema_version,
        args.authored_by_person_id,
        STATUS_EDITED,
    )
    .await?;

    journal(
        db,
        args.notation_id,
        args.authored_by_person_id,
        &document,
        &version,
        args.recorded_at,
        "edited",
    )
    .await?;

    Ok(version)
}

async fn create_document(
    _db: &ApplicationStore,
    notation_id: Uuid,
    original_asset_id: Uuid,
) -> Result<NotationDocument, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(DOCUMENT_TABLE).await?;
    crate::postgres::notation_documents::create(&pg, notation_id, original_asset_id).await
}

/// Append one immutable version row, then compare-and-swap the document's
/// `current_version_id` from `expected_parent` to the new row.
///
/// The version row is always written first and is never deleted, even when
/// the compare-and-swap below loses a race — an append-only table has
/// nothing to roll back. Losing the swap means only that this version never
/// becomes current; the caller sees [`NotationDocumentError::Conflict`] and
/// the winner's version remains readable through [`history`].
#[allow(clippy::too_many_arguments)]
async fn append_version(
    _db: &ApplicationStore,
    document: &NotationDocument,
    expected_parent: Option<Uuid>,
    markdown_asset_id: Uuid,
    anchor_manifest_asset_id: Uuid,
    parser_version: &str,
    schema_version: i64,
    authored_by_person_id: Uuid,
    status: &str,
) -> Result<NotationDocumentVersion, NotationDocumentError> {
    let pg = crate::postgres::matter::authority_for(VERSION_TABLE).await?;
    crate::postgres::notation_documents::append(
        &pg,
        document,
        expected_parent,
        markdown_asset_id,
        anchor_manifest_asset_id,
        parser_version,
        schema_version,
        authored_by_person_id,
        status,
    )
    .await
}

/// Journal identifiers only — never Markdown bytes, never the anchor
/// manifest's contents. `from_state`/`to_state` carry version ids (or the
/// literal `"NONE"` for a root's absent parent) purely as opaque tokens for
/// the shared `notation_events` shape; the payload below is the structured
/// read.
async fn journal(
    db: &ApplicationStore,
    notation_id: Uuid,
    acting_person_id: Uuid,
    document: &NotationDocument,
    version: &NotationDocumentVersion,
    recorded_at: &str,
    condition: &str,
) -> Result<(), NotationDocumentError> {
    let from_state = version
        .parent_version_id
        .map_or_else(|| "NONE".to_string(), |id| id.to_string());
    let payload = serde_json::json!({
        "document_id": document.id,
        "version_id": version.id,
        "parent_version_id": version.parent_version_id,
        "original_asset_id": document.original_asset_id,
        "markdown_asset_id": version.markdown_asset_id,
        "anchor_manifest_asset_id": version.anchor_manifest_asset_id,
        "parser_version": version.parser_version,
        "schema_version": version.schema_version,
        "status": version.status,
    })
    .to_string();
    crate::notation_events::append_event(
        db,
        crate::notation_events::TransitionRecord {
            notation_id,
            acting_person_id: Some(acting_person_id),
            machine_kind: MACHINE_DOCUMENT,
            from_state: &from_state,
            to_state: &version.id.to_string(),
            condition,
            payload_json: Some(payload),
            recorded_at,
        },
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------
// Word-level diff (ENG-579)
//
// No workspace crate does word-level diffing today; `cli::document_read`
// hand-rolls a *line*-level LCS diff for the same reason this one is
// hand-rolled rather than reaching for a dependency: one small, reviewable
// comparison table beats a second diff crate for a text-only need.

/// One span of a [`word_diff`] result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Equal,
    Removed,
    Added,
}

/// One contiguous run of same-kind tokens from [`word_diff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiffSpan {
    pub kind: DiffKind,
    pub text: String,
}

/// Most either side of a word-level diff may carry before [`word_diff`]
/// refuses rather than building an O(tokens²) comparison table.
const MAX_DIFF_TOKENS: usize = 20_000;

#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    #[error(
        "the {side} side has more than {MAX_DIFF_TOKENS} word/whitespace tokens; word_diff \
         refuses rather than building an oversized comparison table"
    )]
    TooLarge { side: &'static str },
}

/// A minimal word-level diff (`Equal`/`Removed`/`Added` spans) via a
/// textbook LCS table, tokenized on whitespace/non-whitespace runs so
/// spacing round-trips exactly. The structural, block-level diff (which
/// blocks moved, which anchors appeared or disappeared) is a property of
/// two versions' anchor manifests, not of this text-only comparison.
///
/// # Errors
/// [`DiffError::TooLarge`] when either side exceeds [`MAX_DIFF_TOKENS`].
pub fn word_diff(old: &str, new: &str) -> Result<Vec<DiffSpan>, DiffError> {
    let left = tokenize(old);
    let right = tokenize(new);
    if left.len() > MAX_DIFF_TOKENS {
        return Err(DiffError::TooLarge { side: "old" });
    }
    if right.len() > MAX_DIFF_TOKENS {
        return Err(DiffError::TooLarge { side: "new" });
    }
    let (left_len, right_len) = (left.len(), right.len());
    let mut lcs = vec![vec![0usize; right_len + 1]; left_len + 1];
    for left_idx in (0..left_len).rev() {
        for right_idx in (0..right_len).rev() {
            lcs[left_idx][right_idx] = if left[left_idx] == right[right_idx] {
                lcs[left_idx + 1][right_idx + 1] + 1
            } else {
                lcs[left_idx + 1][right_idx].max(lcs[left_idx][right_idx + 1])
            };
        }
    }

    let mut spans: Vec<DiffSpan> = Vec::new();
    let push = |spans: &mut Vec<DiffSpan>, kind: DiffKind, text: &str| {
        if let Some(last) = spans.last_mut() {
            if last.kind == kind {
                last.text.push_str(text);
                return;
            }
        }
        spans.push(DiffSpan {
            kind,
            text: text.to_string(),
        });
    };
    let (mut left_idx, mut right_idx) = (0, 0);
    while left_idx < left_len && right_idx < right_len {
        if left[left_idx] == right[right_idx] {
            push(&mut spans, DiffKind::Equal, left[left_idx]);
            left_idx += 1;
            right_idx += 1;
        } else if lcs[left_idx + 1][right_idx] >= lcs[left_idx][right_idx + 1] {
            push(&mut spans, DiffKind::Removed, left[left_idx]);
            left_idx += 1;
        } else {
            push(&mut spans, DiffKind::Added, right[right_idx]);
            right_idx += 1;
        }
    }
    for token in &left[left_idx..] {
        push(&mut spans, DiffKind::Removed, token);
    }
    for token in &right[right_idx..] {
        push(&mut spans, DiffKind::Added, token);
    }
    Ok(spans)
}

/// Split into alternating whitespace-run and non-whitespace-run tokens, so
/// re-joining every token reproduces the input exactly.
fn tokenize(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = s.char_indices().peekable();
    while let Some(&(start, ch)) = chars.peek() {
        let is_ws = ch.is_whitespace();
        chars.next();
        let mut end = start + ch.len_utf8();
        while let Some(&(idx, c)) = chars.peek() {
            if c.is_whitespace() == is_ws {
                end = idx + c.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        tokens.push(&s[start..end]);
    }
    tokens
}

// ---------------------------------------------------------------------
// Protected tokens (ENG-579)

/// A category of value a review policy may lock so an edit cannot change it
/// without the attorney seeing it flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedTokenKind {
    Number,
    Percentage,
    Currency,
    /// `YYYY-MM-DD` or `M/D/YYYY` (and every digit-count in between). A
    /// textual month name (`March 3, 2026`) is not detected — see
    /// [`protected_tokens`]'s doc for the scope this covers.
    Date,
}

/// One protected value found in text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedToken {
    pub kind: ProtectedTokenKind,
    pub text: String,
}

/// A conservative, hand-scanned subset of "protected" legal values: plain
/// numbers, percentages, currency amounts, and two common numeric date
/// shapes. Deliberately not exhaustive — textual month-name dates and
/// defined terms are not detected here. ENG-581 owns the full,
/// independently implemented protected-fact verification gate this
/// exposure feeds into; this function only gives the attorney's editor
/// something concrete to show and lock while a version is being edited.
#[must_use]
pub fn protected_tokens(text: &str) -> Vec<ProtectedToken> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_at = |idx: usize| chars.get(idx).map_or(text.len(), |&(b, _)| b);
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i].1;
        if ch == '$' {
            let digits_end = scan_number(&chars, i + 1);
            if digits_end > i + 1 {
                tokens.push(ProtectedToken {
                    kind: ProtectedTokenKind::Currency,
                    text: text[byte_at(i)..byte_at(digits_end)].to_string(),
                });
                i = digits_end;
                continue;
            }
        } else if ch.is_ascii_digit() {
            if let Some(date_end) = scan_iso_date(&chars, i).or_else(|| scan_slash_date(&chars, i))
            {
                tokens.push(ProtectedToken {
                    kind: ProtectedTokenKind::Date,
                    text: text[byte_at(i)..byte_at(date_end)].to_string(),
                });
                i = date_end;
                continue;
            }
            let mut end = scan_number(&chars, i);
            let kind = if chars.get(end).map(|&(_, c)| c) == Some('%') {
                end += 1;
                ProtectedTokenKind::Percentage
            } else {
                ProtectedTokenKind::Number
            };
            tokens.push(ProtectedToken {
                kind,
                text: text[byte_at(i)..byte_at(end)].to_string(),
            });
            i = end;
            continue;
        }
        i += 1;
    }
    tokens
}

/// Whether any protected token of a locked kind differs — by exact sorted
/// multiset comparison, so reordering the surrounding prose alone is not a
/// change — between `old` and `new`.
#[must_use]
pub fn protected_tokens_changed(old: &str, new: &str, locked: &[ProtectedTokenKind]) -> bool {
    for &kind in locked {
        let mut old_tokens: Vec<String> = protected_tokens(old)
            .into_iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.text)
            .collect();
        let mut new_tokens: Vec<String> = protected_tokens(new)
            .into_iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.text)
            .collect();
        old_tokens.sort();
        new_tokens.sort();
        if old_tokens != new_tokens {
            return true;
        }
    }
    false
}

/// The end index (exclusive, into `chars`) of a number starting at `start`:
/// digits, optionally comma-grouped, optionally one decimal point followed
/// by more digits.
fn scan_number(chars: &[(usize, char)], start: usize) -> usize {
    let mut i = start;
    while chars
        .get(i)
        .is_some_and(|&(_, c)| c.is_ascii_digit() || c == ',')
    {
        i += 1;
    }
    if chars.get(i).map(|&(_, c)| c) == Some('.')
        && chars.get(i + 1).is_some_and(|&(_, c)| c.is_ascii_digit())
    {
        i += 1;
        while chars.get(i).is_some_and(|&(_, c)| c.is_ascii_digit()) {
            i += 1;
        }
    }
    i
}

/// Exactly `min..=max` consecutive ASCII digits from `start`, greedy up to
/// `max`. `None` when fewer than `min` are present.
fn scan_digits(chars: &[(usize, char)], start: usize, min: usize, max: usize) -> Option<usize> {
    let mut i = start;
    while i < chars.len() && i - start < max && chars[i].1.is_ascii_digit() {
        i += 1;
    }
    (i - start >= min).then_some(i)
}

fn expect_char(chars: &[(usize, char)], i: usize, expected: char) -> Option<usize> {
    (chars.get(i).map(|&(_, c)| c) == Some(expected)).then_some(i + 1)
}

/// `YYYY-MM-DD`.
fn scan_iso_date(chars: &[(usize, char)], start: usize) -> Option<usize> {
    let i = scan_digits(chars, start, 4, 4)?;
    let i = expect_char(chars, i, '-')?;
    let i = scan_digits(chars, i, 2, 2)?;
    let i = expect_char(chars, i, '-')?;
    scan_digits(chars, i, 2, 2)
}

/// `M/D/YYYY`, `MM/DD/YY`, and every digit-count in between.
fn scan_slash_date(chars: &[(usize, char)], start: usize) -> Option<usize> {
    let i = scan_digits(chars, start, 1, 2)?;
    let i = expect_char(chars, i, '/')?;
    let i = scan_digits(chars, i, 1, 2)?;
    let i = expect_char(chars, i, '/')?;
    scan_digits(chars, i, 2, 4)
}

// ---------------------------------------------------------------------
// Anchored comments and per-change decisions (ENG-579)
//
// A comment is attached to a specific version and block anchor, so an edit
// to a *later* version cannot silently relocate it — see
// `comments_for_version`. Deciding one is the granular accept/reject/edit
// gate the review surface enforces: there is no bulk-decide export, the
// same structural shape `store::notation_events` uses to pin "append-only,
// no update or delete" (see that module's own `no_update_or_delete_is_exported`
// test) — a reviewer reading this module's public function list alongside
// its tests is the check that no such function was added.

pub(crate) const COMMENT_TABLE: &str = "notation_document_comment";

/// `notation_events.machine_kind` for a comment decision.
pub const MACHINE_DOCUMENT_REVIEW: &str = "document_review";

pub const DECISION_ACCEPTED: &str = "accepted";
pub const DECISION_REJECTED: &str = "rejected";
pub const DECISION_EDITED: &str = "edited";

/// One comment anchored to a block within a specific document version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationDocumentComment {
    pub id: Uuid,
    pub version_id: Uuid,
    pub anchor: String,
    pub person_id: Uuid,
    pub body: String,
    /// `None` until [`decide_comment`] records one.
    pub decision: Option<String>,
    pub resolved: bool,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

async fn comment_authority() -> Result<crate::postgres::PostgresDb, NotationDocumentError> {
    Ok(crate::postgres::matter::authority_for(COMMENT_TABLE).await?)
}

/// Attach a comment to `anchor` within `version_id`. Anyone with write
/// access to the Notation's document may comment; only [`decide_comment`]
/// is the attorney gate.
///
/// # Errors
/// [`NotationDocumentError::ProjectMismatch`] if `notation_id` is not in
/// `project_id`; [`NotationDocumentError::VersionNotFound`] if
/// `version_id` does not belong to this Notation's document; or a database
/// error.
pub async fn add_comment(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    version_id: Uuid,
    anchor: &str,
    person_id: Uuid,
    body: &str,
) -> Result<Uuid, NotationDocumentError> {
    let pg = comment_authority().await?;
    let _ = db;
    let now = Utc::now();
    crate::postgres::notation_document_comments::add(
        &pg,
        project_id,
        notation_id,
        NotationDocumentComment {
            id: Uuid::now_v7(),
            version_id,
            anchor: anchor.to_owned(),
            person_id,
            body: body.to_owned(),
            decision: None,
            resolved: false,
            inserted_at: now,
            updated_at: now,
        },
    )
    .await
}

/// Every comment anchored to `version_id`, oldest first.
///
/// # Errors
/// [`NotationDocumentError::ProjectMismatch`] if `notation_id` is not in
/// `project_id`, or a database error.
pub async fn comments_for_version(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    version_id: Uuid,
) -> Result<Vec<NotationDocumentComment>, NotationDocumentError> {
    let pg = comment_authority().await?;
    let _ = db;
    crate::postgres::notation_document_comments::list(&pg, project_id, notation_id, version_id)
        .await
}

/// Carry comments forward when an attorney saves a child version. Anchors
/// that still exist remain resolved; anchors that disappeared are copied with
/// `resolved = false` so the review surface can show the unresolved finding
/// instead of silently attaching it to nearby text.
pub async fn carry_comments_to_version(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    parent_version_id: Uuid,
    child_version_id: Uuid,
    anchors: &[String],
) -> Result<Vec<NotationDocumentComment>, NotationDocumentError> {
    let pg = comment_authority().await?;
    let _ = db;
    crate::postgres::notation_document_comments::carry(
        &pg,
        project_id,
        notation_id,
        parent_version_id,
        child_version_id,
        anchors,
    )
    .await
}

/// Why [`decide_comment`] refused.
#[derive(Debug, thiserror::Error)]
pub enum DecideCommentError {
    #[error("notation document: {0}")]
    Document(#[from] NotationDocumentError),
    #[error("comment {0} not found")]
    NotFound(Uuid),
    #[error("comment {0} already carries a decision")]
    AlreadyDecided(Uuid),
    #[error("`{0}` is not accepted, rejected, or edited")]
    InvalidDecision(String),
}

/// Record one attorney decision on one comment — `accepted`, `rejected`, or
/// `edited`. Exactly one call decides exactly one comment; there is no
/// bulk-decide entry point (see the module doc). A comment that already
/// carries a decision refuses rather than silently overwriting it.
///
/// # Errors
/// See [`DecideCommentError`].
pub async fn decide_comment(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    comment_id: Uuid,
    decision: &str,
    acting_person_id: Uuid,
    recorded_at: &str,
) -> Result<(), DecideCommentError> {
    if ![DECISION_ACCEPTED, DECISION_REJECTED, DECISION_EDITED].contains(&decision) {
        return Err(DecideCommentError::InvalidDecision(decision.to_string()));
    }
    let pg = comment_authority()
        .await
        .map_err(DecideCommentError::Document)?;
    let _ = db;
    crate::postgres::notation_document_comments::decide(
        &pg,
        project_id,
        crate::notation_events::TransitionRecord {
            notation_id,
            acting_person_id: Some(acting_person_id),
            machine_kind: MACHINE_DOCUMENT_REVIEW,
            from_state: "pending",
            to_state: decision,
            condition: "decided",
            payload_json: None,
            recorded_at,
        },
        comment_id,
    )
    .await
}

/// Whether every comment anchored to `version_id` carries a decision — the
/// per-version "nothing left to act on" read the review surface gates
/// advancing on.
///
/// # Errors
/// [`NotationDocumentError::ProjectMismatch`] if `notation_id` is not in
/// `project_id`, or a database error.
pub async fn all_comments_decided(
    db: &ApplicationStore,
    project_id: Uuid,
    notation_id: Uuid,
    version_id: Uuid,
) -> Result<bool, NotationDocumentError> {
    let comments = comments_for_version(db, project_id, notation_id, version_id).await?;
    Ok(comments.iter().all(|c| c.decision.is_some()))
}

#[cfg(test)]
mod tests {}

#[cfg(test)]
mod diff_tests {
    use super::{word_diff, DiffError, DiffKind, DiffSpan};

    fn equal(text: &str) -> DiffSpan {
        DiffSpan {
            kind: DiffKind::Equal,
            text: text.to_string(),
        }
    }
    fn removed(text: &str) -> DiffSpan {
        DiffSpan {
            kind: DiffKind::Removed,
            text: text.to_string(),
        }
    }
    fn added(text: &str) -> DiffSpan {
        DiffSpan {
            kind: DiffKind::Added,
            text: text.to_string(),
        }
    }

    #[test]
    fn identical_text_is_one_equal_span() {
        assert_eq!(
            word_diff("the quick fox", "the quick fox").unwrap(),
            vec![equal("the quick fox")]
        );
    }

    #[test]
    fn a_single_word_substitution_is_removed_then_added() {
        let spans = word_diff("the quick fox jumps", "the slow fox jumps").unwrap();
        assert_eq!(
            spans,
            vec![
                equal("the "),
                removed("quick"),
                added("slow"),
                equal(" fox jumps"),
            ]
        );
    }

    #[test]
    fn an_appended_sentence_is_a_trailing_added_span() {
        let spans = word_diff("Clause one.", "Clause one. Clause two.").unwrap();
        assert_eq!(spans, vec![equal("Clause one."), added(" Clause two.")]);
    }

    #[test]
    fn reassembling_every_span_reproduces_either_side() {
        let old = "the quick brown fox";
        let new = "the slow brown fox jumps";
        let spans = word_diff(old, new).unwrap();
        let reassembled_old: String = spans
            .iter()
            .filter(|s| s.kind != DiffKind::Added)
            .map(|s| s.text.as_str())
            .collect();
        let reassembled_new: String = spans
            .iter()
            .filter(|s| s.kind != DiffKind::Removed)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(reassembled_old, old);
        assert_eq!(reassembled_new, new);
    }

    #[test]
    fn an_oversized_side_is_refused_rather_than_compared() {
        let huge = "word ".repeat(super::MAX_DIFF_TOKENS);
        assert!(matches!(
            word_diff(&huge, "short"),
            Err(DiffError::TooLarge { side: "old" })
        ));
    }
}

#[cfg(test)]
mod protected_token_tests {
    use super::{protected_tokens, protected_tokens_changed, ProtectedToken, ProtectedTokenKind};

    #[test]
    fn finds_a_plain_number() {
        let tokens = protected_tokens("liability is capped at 1,000,000 units");
        assert_eq!(
            tokens,
            vec![ProtectedToken {
                kind: ProtectedTokenKind::Number,
                text: "1,000,000".into(),
            }]
        );
    }

    #[test]
    fn finds_a_currency_amount() {
        let tokens = protected_tokens("a fee of $2,500.00 is due");
        assert_eq!(
            tokens,
            vec![ProtectedToken {
                kind: ProtectedTokenKind::Currency,
                text: "$2,500.00".into(),
            }]
        );
    }

    #[test]
    fn finds_a_percentage() {
        let tokens = protected_tokens("interest accrues at 5.5% per annum");
        assert_eq!(
            tokens,
            vec![ProtectedToken {
                kind: ProtectedTokenKind::Percentage,
                text: "5.5%".into(),
            }]
        );
    }

    #[test]
    fn finds_iso_and_slash_dates() {
        let tokens = protected_tokens("effective 2026-03-05, expiring 3/5/2027");
        assert_eq!(
            tokens,
            vec![
                ProtectedToken {
                    kind: ProtectedTokenKind::Date,
                    text: "2026-03-05".into(),
                },
                ProtectedToken {
                    kind: ProtectedTokenKind::Date,
                    text: "3/5/2027".into(),
                },
            ]
        );
    }

    #[test]
    fn a_changed_locked_number_is_detected() {
        let old = "the cap is 1,000,000 dollars";
        let new = "the cap is 2,000,000 dollars";
        assert!(protected_tokens_changed(
            old,
            new,
            &[ProtectedTokenKind::Number]
        ));
    }

    #[test]
    fn reordering_prose_around_an_unchanged_number_is_not_a_change() {
        let old = "Section 9: the cap is 1,000,000 dollars, effective immediately.";
        let new = "Effective immediately, the cap under Section 9 is 1,000,000 dollars.";
        assert!(!protected_tokens_changed(
            old,
            new,
            &[ProtectedTokenKind::Number]
        ));
    }

    #[test]
    fn an_unlocked_kind_is_never_flagged() {
        let old = "the cap is 1,000,000 dollars";
        let new = "the cap is 2,000,000 dollars";
        assert!(!protected_tokens_changed(
            old,
            new,
            &[ProtectedTokenKind::Currency]
        ));
    }
}
