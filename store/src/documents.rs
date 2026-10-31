//! `store::documents` — write-side primitive for **document assets**.
//!
//! A matter document (a portal upload, an inbound-email attachment, a
//! generated PDF) is one `asset` row carrying both the byte pointer and
//! the document metadata. [`ingest_bytes`] is the one entry point. Given
//! a project, bytes, and provenance, it:
//!
//! 1. Computes a SHA-256 of the bytes.
//! 2. If an `asset` row **on this project** already references that content,
//!    the object is already stored beneath
//!    `projects/<code>/documents/<sha>` — the storage write is skipped.
//!    Otherwise it writes the bytes through [`cloud::StorageService`].
//! 3. Inserts one `asset` row carrying the byte pointer plus the
//!    inbound-channel provenance (`source`, `received_at`), the
//!    `filename`/`kind`, and the optional lawyer-view `description`.
//!
//! # Ordering, not a transaction
//!
//! A multi-statement Surreal query is not one transaction, so steps 2–3
//! do not share one. They do not need to. The guarantee to hold is "never
//! leave a row pointing at a storage key
//! that does not exist", and that is a property of the **order**: the
//! object is written before the row that names it, so a failure between
//! them leaves an unreferenced object rather than a dangling row. An
//! unreferenced object at a Project-scoped content-addressed key is inert —
//! the next ingest of the same bytes reuses it.
//!
//! The dedup probe losing a race is equally benign: both writers then
//! `put` identical bytes to the same Project-scoped content-addressed key,
//! and `put` is idempotent on both `FsStorage` and `GcsStorage`. The only
//! observable difference is a redundant write, never a wrong byte.
//!
//! The bare-content lane (template bodies, raw `.eml`) is
//! [`crate::assets::ingest_content`], which writes an `asset` row with
//! the document-metadata fields left unset.

use std::sync::Arc;
use std::{fmt, path::Path};

use chrono::{DateTime, Utc};
use cloud::{StorageError, StorageService};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::assets::{sha256_hex as asset_sha256_hex, SELECT, TABLE};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// Maximum size of one document accepted by the document upload lane.
///
/// Twenty-five MiB matches the size mail systems accept for real instruments
/// while keeping the buffered REST upload bounded until the lane can stream.
/// Keep this limit here so every document-ingest caller shares one contract.
pub const MAX_DOCUMENT_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// Maximum size of one transcript document, the kind that carries court
/// hearing audio (LAW-172).
///
/// A two-hour hearing recording docketed as a PDF wrapper runs about 60 MB, and
/// the bytes of a one-time court free look cannot be re-encoded or re-fetched.
/// One hundred MiB leaves room for longer hearings while keeping the buffered
/// base64 request bounded (about 134 MB). Larger media uploads through the
/// resumable streaming lane ([`MAX_STREAMING_UPLOAD_BYTES`]), not this door.
pub const MAX_MEDIA_DOCUMENT_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// The document-size limit that applies to a document `kind`.
#[must_use]
pub fn max_document_upload_bytes(kind: &str) -> usize {
    if kind == "transcript" {
        MAX_MEDIA_DOCUMENT_UPLOAD_BYTES
    } else {
        MAX_DOCUMENT_UPLOAD_BYTES
    }
}

/// Maximum REST request size needed to carry the document limit as base64 JSON.
///
/// The request body is larger than the document bytes because the REST door
/// carries the content as base64 plus a small JSON envelope. The expansion
/// uses `div_ceil` so a maximum-size document does not spend JSON allowance
/// on the encoding.
pub const MAX_DOCUMENT_UPLOAD_REQUEST_BYTES: usize =
    MAX_MEDIA_DOCUMENT_UPLOAD_BYTES.div_ceil(3) * 4 + MAX_DOCUMENT_UPLOAD_JSON_BYTES;

/// Room the request cap leaves for the upload's own JSON fields.
///
/// Everything in the request that is not the encoded document: `filename`,
/// `kind`, `visibility`, `description`, `slug`, and the free-form `metadata`
/// the caller passes through verbatim.
pub const MAX_DOCUMENT_UPLOAD_JSON_BYTES: usize = 4096;

/// The fixed chunk size one streaming-lane append must carry (LAW-172).
///
/// Advertised by the session row, never chosen by the client, so a single
/// append request is bounded by [`MAX_UPLOAD_CHUNK_REQUEST_BYTES`] no matter
/// what the upload's total size is.
pub const STREAMING_UPLOAD_CHUNK_SIZE: usize = 8 * 1024 * 1024;

/// The largest document the streaming lane accepts.
///
/// One GiB covers court hearing audio (the LAW-172 report is ~60 MB) and
/// video exhibits while bounding the lane's one whole-object materialization:
/// `StorageService::put` writes an object from one buffer, so composing the
/// staged parts at completion holds the full file once. Above this the
/// upload is refused with [`streaming_upload_size_message`] rather than
/// half-bounded.
pub const MAX_STREAMING_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;

/// Maximum REST request size needed to carry one streaming chunk as base64
/// JSON — the same `div_ceil` expansion the buffered door's cap uses.
pub const MAX_UPLOAD_CHUNK_REQUEST_BYTES: usize =
    STREAMING_UPLOAD_CHUNK_SIZE.div_ceil(3) * 4 + MAX_DOCUMENT_UPLOAD_JSON_BYTES;

/// How long an upload session stays usable, from `created_at`. Long enough
/// for a multi-hundred-MB file on a slow link; short enough that abandoned
/// rows are finite. An expired session accepts no chunk and cannot complete;
/// re-running the upload opens a fresh session.
pub const UPLOAD_SESSION_TTL_HOURS: i64 = 24;

/// Explain a refusal past the streaming lane's own cap — the one size the
/// lane itself cannot take.
#[must_use]
pub fn streaming_upload_size_message(actual: usize) -> String {
    format!(
        "Document upload exceeds the streaming lane's maximum of \
         {MAX_STREAMING_UPLOAD_BYTES} bytes; received {actual} bytes. No Navigator \
         upload path accepts more; the file must be split."
    )
}

/// Explain a document-size rejection for `kind`: the limit that applies, the
/// actual size, and where an oversized file should go instead.
#[must_use]
pub fn document_upload_size_message_for_kind(kind: &str, actual: usize) -> String {
    let max = max_document_upload_bytes(kind);
    let next = if kind == "transcript" {
        "Files over this limit upload through the resumable streaming lane, which `navigator site document upload` routes automatically."
    } else {
        "Court hearing audio and similar recordings may be filed with `--kind transcript`, up to 104857600 bytes."
    };
    format!("Document upload exceeds the maximum of {max} bytes; received {actual} bytes. {next}")
}

/// The Pointer path must retain the uploaded filename's extension.
/// A slug without one cannot be represented as `blobs/<slug>.yaml` while
/// also satisfying the Project gate's pointer rule.
#[derive(Debug, PartialEq, Eq)]
pub enum DocumentSlugError {
    MissingOrMismatchedExtension { filename: String, slug: String },
}

impl fmt::Display for DocumentSlugError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrMismatchedExtension { filename, slug } => {
                if let Some(suggested_slug) = suggested_slug(filename, slug) {
                    write!(
                        formatter,
                        "document slug `{slug}` must carry the source extension from `{filename}`; use `{suggested_slug}` so it can be represented as a Pointer"
                    )
                } else {
                    write!(
                        formatter,
                        "document slug `{slug}` must carry the source extension from `{filename}` so it can be represented as a Pointer"
                    )
                }
            }
        }
    }
}

impl std::error::Error for DocumentSlugError {}

fn suggested_slug(filename: &str, slug: &str) -> Option<String> {
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())?;
    let stem = slug
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/') && !stem.ends_with('\\'))
        .unwrap_or(slug);
    Some(format!("{stem}.{extension}"))
}

/// Reject a document identity that cannot be represented by a repository
/// pointer. Upload callers share this check so a pointer is never issued that
/// the local gate would necessarily reject.
pub fn validate_document_slug(filename: &str, slug: &str) -> Result<(), DocumentSlugError> {
    let filename_extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());
    let slug_extension = Path::new(slug)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());

    if filename_extension.is_some_and(|expected| {
        slug_extension.is_some_and(|actual| expected.eq_ignore_ascii_case(actual))
    }) {
        Ok(())
    } else {
        Err(DocumentSlugError::MissingOrMismatchedExtension {
            filename: filename.to_string(),
            slug: slug.to_string(),
        })
    }
}

/// Inbound-channel literals written to `assets.source`. Centralized here
/// so handlers and tests agree on the same strings; mismatches turn into
/// silent dedup-planner bugs.
pub mod source {
    pub const UPLOAD: &str = "upload";
    /// An attachment received on inbound `support@` mail (see
    /// `portal::email_threads`).
    pub const EMAIL: &str = "email";
    /// A document the workflow engine rendered itself — the PDF a
    /// `generate_pdf__*` step produces (retainer, trust, filled
    /// government packet). Not an inbound channel; the bytes originate
    /// in-house from the template + answers.
    pub const GENERATED: &str = "generated";
    /// A file copied from the Project's Google Drive ingest folder.
    pub const DRIVE: &str = "drive";
}

/// Matter-page visibility literals written to `assets.visibility`. A
/// closed vocabulary enforced by the schema ASSERT on `asset.visibility`;
/// centralized here so every caller states its intent explicitly rather
/// than falling back to a silent default (#782).
pub mod visibility {
    /// Lawyer-only — the default. Attorney work product (`review_memo`) and
    /// `unclassified` lawyer/email uploads.
    pub const INTERNAL: &str = "internal";
    /// Listed in the client's "Your documents".
    pub const CLIENT: &str = "client";
}

// The resumable streaming lane for documents larger than the buffered door
// accepts (LAW-172).
//
// Court audio or a video exhibit cannot travel as one base64 JSON request:
// the buffered door's request cap bounds a single REST body, and the bytes
// of a one-time free look cannot be re-fetched to retry it. The lane instead
// carries the upload as an `UploadSession` — a durable row that counts how
// many bytes have durably arrived — plus one staged part object per chunk.
// The client streams server-sized chunks (`STREAMING_UPLOAD_CHUNK_SIZE`,
// advertised by the session, never chosen by the client), and every append
// lands at exactly the offset `received_bytes` names, so a dropped
// connection resumes by asking the status door where the server stands
// instead of restarting a file that cannot be re-fetched.
//
// Completing a session composes the staged parts in offset order, verifies
// the whole-file SHA-256 against the digest the client declared at create
// time (the guarantee the issue asks for: the stored bytes hash to the
// original bytes), and files the document through the ordinary
// `crate::assets::file_revision` ingest — the same per-project dedup,
// storage-before-row ordering, and revision rules as every other upload.
// The staged parts and the session row are then removed; until cleanup
// succeeds, a retry of the completion is idempotent
// (`crate::assets::Filed::Unchanged`).
//
// The lane is deliberately not restricted to sizes the buffered door
// refuses: its contract is `1..=MAX_STREAMING_UPLOAD_BYTES` bytes for any
// asset-lane kind, which keeps the boundary testable and each door honest
// about its own limit. Routing by size is the CLI's convenience
// (`upload_lane_for` in `cli::remote`), not a server rule.
//
// # Ordering, not a transaction
//
// The same guarantee the buffered lane holds applies twice over: a part
// object is written before the row that counts it, and the composed object
// is written (by `file_revision`) before the session row and parts are
// removed. A failure between the steps leaves inert staged content or a
// resumable session, never a row pointing at bytes that are not there.

/// The table holding upload sessions.
const UPLOAD_SESSION_TABLE: &str = "document_upload_session";

/// The upload session's columns, in the read order every accessor shares.
const UPLOAD_SESSION_SELECT: &str = "id, project_id, filename, kind, content_type, \
     visibility, description, slug, transcript_quality, metadata, derived_from, \
     signing, notarization, declared_size, declared_sha256, chunk_size, \
     received_bytes, created_at, expires_at";

/// One in-flight streaming upload: the document the caller means to file and
/// the identity of its bytes, both as declared at create time, plus how many
/// bytes have arrived. The row is the resume point — a client that loses its
/// connection reads it and continues at `received_bytes`.
#[derive(Debug, Clone, PartialEq)]
pub struct UploadSession {
    pub id: Uuid,
    pub project_id: Uuid,
    pub filename: String,
    pub kind: String,
    pub content_type: String,
    pub visibility: String,
    pub description: Option<String>,
    pub slug: Option<String>,
    pub transcript_quality: Option<String>,
    pub metadata: Option<Json>,
    pub derived_from: Option<Json>,
    pub signing: Option<Json>,
    pub notarization: Option<Json>,
    /// The byte size the client declared for the whole file. The last chunk
    /// must close on exactly this.
    pub declared_size: i64,
    /// Lowercase hex SHA-256 of the whole original file, verified at
    /// completion against the composed staged bytes.
    pub declared_sha256: String,
    /// The chunk size every append must carry (until the last). Advertised
    /// by the server so one request stays bounded.
    pub chunk_size: i64,
    /// Bytes durably staged so far. The resume point.
    pub received_bytes: i64,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Why a streaming upload step was refused.
#[derive(Debug, thiserror::Error)]
pub enum UploadSessionError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Asset(#[from] crate::assets::AssetError),
    #[error(transparent)]
    Revision(#[from] crate::assets::RevisionError),
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
    #[error("`{0}` is not a valid asset-lane kind")]
    InvalidKind(String),
    #[error("a filename is required")]
    BlankFilename,
    #[error("visibility must be `internal` or `client`")]
    InvalidVisibility,
    #[error("declared sha256 `{0}` must be 64 lowercase hex characters")]
    ShaFormat(String),
    #[error("declared upload size {declared} must be at least one byte")]
    EmptyUpload { declared: i64 },
    #[error("declared upload size {declared} exceeds the streaming lane's maximum of {max} bytes")]
    TooLarge { declared: i64, max: usize },
    #[error("no upload session `{0}` on this matter")]
    NoSuchSession(Uuid),
    #[error("upload session expired at {0}")]
    Expired(String),
    #[error("the upload already holds every declared byte")]
    AlreadyComplete,
    #[error("next append must carry exactly {expected} bytes at offset {offset}, not {actual}")]
    ChunkLength {
        offset: i64,
        expected: i64,
        actual: i64,
    },
    #[error("upload is incomplete: {received} of {declared} bytes received")]
    Incomplete { received: i64, declared: i64 },
    #[error("staged bytes hash to {stored}, not the declared {declared}; restart the upload")]
    ShaMismatch { stored: String, declared: String },
    #[error("stored upload session is corrupt: {0}")]
    CorruptDetail(String),
}

/// Inputs to [`create_upload_session`] — the document the caller means to
/// file, and the identity of its bytes. The same document-metadata vocabulary
/// as [`IngestArgs`] and [`DocumentIdentity`], because completing the session
/// files the bytes through exactly that ingest.
#[derive(Debug, Clone)]
pub struct UploadSessionArgs<'a> {
    pub project_id: Uuid,
    pub filename: &'a str,
    pub kind: &'a str,
    pub content_type: &'a str,
    pub visibility: &'a str,
    pub description: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub transcript_quality: Option<&'a str>,
    pub metadata: Option<Json>,
    pub derived_from: Option<Json>,
    pub signing: Option<Json>,
    pub notarization: Option<Json>,
    pub declared_size: usize,
    pub declared_sha256: &'a str,
}

/// The byte length the session's next append must carry: a full
/// [`STREAMING_UPLOAD_CHUNK_SIZE`] until the declared size closes it out,
/// then the short remainder. `None` when every declared byte has arrived.
#[must_use]
pub fn next_chunk_len(declared_size: i64, received_bytes: i64, chunk_size: i64) -> Option<i64> {
    if received_bytes >= declared_size {
        return None;
    }
    Some(chunk_size.min(declared_size - received_bytes))
}

/// Validate one append against the session and return the byte count it will
/// hold afterward. Strictly sequential: the chunk must be exactly what
/// [`next_chunk_len`] names, which makes `received_bytes` the resume point —
/// a chunk the server already applied is refused with the offset it stands
/// at (and re-reported by the status door), never applied twice. Expiry is
/// checked first, so an expired session refuses even a perfect chunk.
///
/// # Errors
/// [`UploadSessionError::Expired`], [`UploadSessionError::AlreadyComplete`],
/// or [`UploadSessionError::ChunkLength`].
pub fn check_append(
    session: &UploadSession,
    now: DateTime<Utc>,
    chunk_len: usize,
) -> Result<i64, UploadSessionError> {
    let actual = i64::try_from(chunk_len).unwrap_or(i64::MAX);
    if now > session.expires_at {
        return Err(UploadSessionError::Expired(session.expires_at.to_rfc3339()));
    }
    let Some(expected) = next_chunk_len(
        session.declared_size,
        session.received_bytes,
        session.chunk_size,
    ) else {
        return Err(UploadSessionError::AlreadyComplete);
    };
    if actual != expected {
        return Err(UploadSessionError::ChunkLength {
            offset: session.received_bytes,
            expected,
            actual,
        });
    }
    Ok(session.received_bytes + expected)
}

/// The staged-object key for one chunk of a session: offset-named and
/// zero-padded, so the parts sort numerically and a re-sent chunk would name
/// the same key as the one it overwrites.
#[must_use]
pub fn upload_part_key(session_id: Uuid, offset: i64) -> String {
    format!("uploads/{session_id}/part-{offset:020}")
}

/// Open (or resume) a streaming upload session: validate the declared
/// identity, then return the live session for it — the existing row when
/// this exact upload is already in flight, so a crashed client run resumes
/// instead of orphaning a session, else a fresh one.
///
/// # Errors
/// [`UploadSessionError`] on an invalid kind, visibility, digest, or size,
/// or a database failure.
pub async fn create_upload_session(
    db: &SurrealDb,
    args: &UploadSessionArgs<'_>,
) -> Result<UploadSession, UploadSessionError> {
    if !rules::kind::Kind::parse(args.kind).is_some_and(|k| k.valid_for(rules::kind::Lane::Asset)) {
        return Err(UploadSessionError::InvalidKind(args.kind.to_string()));
    }
    if args.filename.trim().is_empty() {
        return Err(UploadSessionError::BlankFilename);
    }
    if args.visibility != visibility::INTERNAL && args.visibility != visibility::CLIENT {
        return Err(UploadSessionError::InvalidVisibility);
    }
    if args.declared_sha256.len() != 64
        || !args
            .declared_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(UploadSessionError::ShaFormat(
            args.declared_sha256.to_string(),
        ));
    }
    let declared_size = i64::try_from(args.declared_size).unwrap_or(i64::MAX);
    if declared_size == 0 {
        return Err(UploadSessionError::EmptyUpload { declared: 0 });
    }
    if declared_size > i64::try_from(MAX_STREAMING_UPLOAD_BYTES).unwrap_or(i64::MAX) {
        return Err(UploadSessionError::TooLarge {
            declared: declared_size,
            max: MAX_STREAMING_UPLOAD_BYTES,
        });
    }

    let now = Utc::now();
    if let Some(existing) = find_resumable_session(db, args, now).await? {
        return Ok(existing);
    }
    let id = Uuid::now_v7();
    let expires_at = now + chrono::Duration::hours(UPLOAD_SESSION_TTL_HOURS);
    crate::assets::writing(|| {
        db.query(format!(
            "CREATE $id SET \
             project_id = $project_id, \
             filename = $filename, \
             kind = $kind, \
             content_type = $content_type, \
             visibility = $visibility, \
             description = $description, \
             slug = $slug, \
             transcript_quality = $transcript_quality, \
             metadata = $metadata, \
             derived_from = $derived_from, \
             signing = $signing, \
             notarization = $notarization, \
             declared_size = $declared_size, \
             declared_sha256 = $declared_sha256, \
             chunk_size = $chunk_size, \
             received_bytes = 0, \
             created_at = $created_at, \
             expires_at = $expires_at \
             RETURN {UPLOAD_SESSION_SELECT}"
        ))
        .bind(("id", record_id(UPLOAD_SESSION_TABLE, id)))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, args.project_id),
        ))
        .bind(("filename", args.filename.trim().to_string()))
        .bind(("kind", args.kind.to_string()))
        .bind(("content_type", args.content_type.to_string()))
        .bind(("visibility", args.visibility.to_string()))
        .bind(("description", args.description.map(String::from)))
        .bind(("slug", args.slug.map(String::from)))
        .bind((
            "transcript_quality",
            args.transcript_quality.map(String::from),
        ))
        .bind(("metadata", args.metadata.clone()))
        .bind(("derived_from", args.derived_from.clone()))
        .bind(("signing", args.signing.clone()))
        .bind(("notarization", args.notarization.clone()))
        .bind(("declared_size", declared_size))
        .bind(("declared_sha256", args.declared_sha256.to_string()))
        .bind((
            "chunk_size",
            i64::try_from(STREAMING_UPLOAD_CHUNK_SIZE).unwrap_or(i64::MAX),
        ))
        .bind(("created_at", surrealdb::types::Datetime::from(now)))
        .bind(("expires_at", surrealdb::types::Datetime::from(expires_at)))
    })
    .await?;

    // Return the session as the client needs it. The create wrote every
    // field from the validated arguments, so the reply is the same session
    // a read-back would return.
    Ok(UploadSession {
        id,
        project_id: args.project_id,
        filename: args.filename.trim().to_string(),
        kind: args.kind.to_string(),
        content_type: args.content_type.to_string(),
        visibility: args.visibility.to_string(),
        description: args.description.map(String::from),
        slug: args.slug.map(String::from),
        transcript_quality: args.transcript_quality.map(String::from),
        metadata: args.metadata.clone(),
        derived_from: args.derived_from.clone(),
        signing: args.signing.clone(),
        notarization: args.notarization.clone(),
        declared_size,
        declared_sha256: args.declared_sha256.to_string(),
        chunk_size: i64::try_from(STREAMING_UPLOAD_CHUNK_SIZE).unwrap_or(i64::MAX),
        received_bytes: 0,
        created_at: now,
        expires_at,
    })
}

/// The live session this exact upload already has, if any: same matter,
/// same filename, same declared size and digest, not yet expired. Rows are
/// removed on completion, so any survivor is in flight.
async fn find_resumable_session(
    db: &SurrealDb,
    args: &UploadSessionArgs<'_>,
    now: DateTime<Utc>,
) -> Result<Option<UploadSession>, UploadSessionError> {
    let mut response = db
        .query(format!(
            "SELECT {UPLOAD_SESSION_SELECT} FROM {UPLOAD_SESSION_TABLE} \
             WHERE project_id = $project_id AND filename = $filename \
             AND declared_size = $declared_size \
             AND declared_sha256 = $declared_sha256 \
             AND expires_at > $now ORDER BY created_at DESC LIMIT 1"
        ))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, args.project_id),
        ))
        .bind(("filename", args.filename.trim().to_string()))
        .bind((
            "declared_size",
            i64::try_from(args.declared_size).unwrap_or(i64::MAX),
        ))
        .bind(("declared_sha256", args.declared_sha256.to_string()))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<UploadSessionRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(UploadSessionRow::into_upload_session)
        .next())
}

/// Read one upload session by id, scoped to the matter. `None` when the id
/// names no session on this project — a finished or foreign session is
/// indistinguishable from a missing one, which is what the doors report.
///
/// # Errors
/// Database failure.
pub async fn find_upload_session(
    db: &SurrealDb,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<Option<UploadSession>, UploadSessionError> {
    let mut response = db
        .query(format!(
            "SELECT {UPLOAD_SESSION_SELECT} FROM ONLY $id LIMIT 1"
        ))
        .bind(("id", record_id(UPLOAD_SESSION_TABLE, session_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<UploadSessionRow> = response.take(0)?;
    Ok(row
        .and_then(UploadSessionRow::into_upload_session)
        .filter(|session| session.project_id == project_id))
}

/// Stage one chunk at exactly the offset the session counts, then advance
/// the count. The part object is written before the row moves (the
/// ordering guarantee the module doc holds: never claim bytes that are not
/// durable), and the chunk length must be exactly [`next_chunk_len`] — the
/// strictly sequential rule that makes `received_bytes` the resume point.
///
/// # Errors
/// [`UploadSessionError`] when the session is missing, expired, complete,
/// or the chunk is not the one the offset names; database and storage
/// failures propagate.
pub async fn append_upload_chunk(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    project_id: Uuid,
    session_id: Uuid,
    chunk: &[u8],
) -> Result<UploadSession, UploadSessionError> {
    let mut session = find_upload_session(db, project_id, session_id)
        .await?
        .ok_or(UploadSessionError::NoSuchSession(session_id))?;
    let new_received = check_append(&session, Utc::now(), chunk.len())?;
    let key = upload_part_key(session_id, session.received_bytes);
    storage.put(&key, chunk, "application/octet-stream").await?;
    crate::assets::writing(|| {
        db.query("UPDATE $id SET received_bytes = $received_bytes")
            .bind(("id", record_id(UPLOAD_SESSION_TABLE, session_id)))
            .bind(("received_bytes", new_received))
    })
    .await?;
    session.received_bytes = new_received;
    Ok(session)
}

/// Compose the staged parts, verify them against the digest declared at
/// create time, and file the document through the ordinary ingest
/// ([`crate::assets::file_revision`]). On success the staged parts and the
/// session row go away; a cleanup failure is logged and left inert rather
/// than failing an upload whose asset is already filed — a retry of this
/// call then reports the existing row ([`crate::assets::Filed::Unchanged`])
/// until the row is gone, after which it reports
/// [`UploadSessionError::NoSuchSession`].
///
/// # Errors
/// [`UploadSessionError`] when the session is missing, expired, or
/// incomplete; [`UploadSessionError::ShaMismatch`] when the staged bytes do
/// not hash to the declared digest (the session and its parts are removed,
/// so a retry starts clean); database, storage, and ingest failures
/// propagate.
pub async fn complete_upload(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    project_id: Uuid,
    session_id: Uuid,
) -> Result<crate::assets::Filed, UploadSessionError> {
    let session = find_upload_session(db, project_id, session_id)
        .await?
        .ok_or(UploadSessionError::NoSuchSession(session_id))?;
    if Utc::now() > session.expires_at {
        return Err(UploadSessionError::Expired(session.expires_at.to_rfc3339()));
    }
    if session.received_bytes != session.declared_size {
        return Err(UploadSessionError::Incomplete {
            received: session.received_bytes,
            declared: session.declared_size,
        });
    }
    let chunk_size = usize::try_from(session.chunk_size).unwrap_or(STREAMING_UPLOAD_CHUNK_SIZE);
    let declared = usize::try_from(session.declared_size).map_err(|_| {
        UploadSessionError::CorruptDetail("declared size does not fit in memory".to_string())
    })?;
    let mut bytes = Vec::with_capacity(declared);
    let mut offset = 0_i64;
    while offset < session.declared_size {
        let object = storage.get(&upload_part_key(session_id, offset)).await?;
        bytes.extend_from_slice(&object.bytes);
        offset += i64::try_from(chunk_size).unwrap_or(i64::MAX);
    }
    let stored = sha256_hex(&bytes);
    if stored != session.declared_sha256 {
        remove_upload_parts(storage, session_id, session.declared_size, chunk_size).await;
        remove_upload_session_row(db, session_id).await;
        return Err(UploadSessionError::ShaMismatch {
            stored,
            declared: session.declared_sha256,
        });
    }

    let filed = crate::assets::file_revision(
        db,
        storage,
        &upload_ingest_args(&session),
        &upload_identity(&session)?,
        &bytes,
    )
    .await?;
    remove_upload_parts(storage, session_id, session.declared_size, chunk_size).await;
    remove_upload_session_row(db, session_id).await;
    Ok(filed)
}

/// The staged ingest arguments for a completed session — the ordinary
/// `upload` channel, so the filed document reads exactly like one the
/// buffered door filed.
fn upload_ingest_args(session: &UploadSession) -> IngestArgs<'_> {
    IngestArgs {
        project_id: session.project_id,
        source: source::UPLOAD,
        filename: &session.filename,
        kind: &session.kind,
        content_type: &session.content_type,
        description: session.description.as_deref(),
        secondary_storage_key: None,
        visibility: &session.visibility,
    }
}

/// The document identity the session carried, parsed back into the typed
/// shapes the ingest writes. Only corruption of an already-validated row
/// fails here.
fn upload_identity(session: &UploadSession) -> Result<DocumentIdentity<'_>, UploadSessionError> {
    Ok(DocumentIdentity {
        slug: session.slug.as_deref(),
        published_at: None,
        metadata: session.metadata.clone(),
        derived_from: session.derived_from.clone(),
        transcript_quality: session.transcript_quality.as_deref(),
        signing: session
            .signing
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| UploadSessionError::CorruptDetail(format!("signing: {error}")))?,
        notarization: session
            .notarization
            .as_ref()
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .map_err(|error| UploadSessionError::CorruptDetail(format!("notarization: {error}")))?
            .unwrap_or_default(),
        evidence: None,
    })
}

/// Best-effort removal of a session's staged parts, sweeping every offset
/// the declared size could have staged (including a part a failed row
/// update left behind). A failure is logged and left for parity: the parts
/// are inert content under the session's own key.
async fn remove_upload_parts(
    storage: &Arc<dyn StorageService>,
    session_id: Uuid,
    declared_size: i64,
    chunk_size: usize,
) {
    let chunk = i64::try_from(chunk_size).unwrap_or(i64::MAX);
    let mut offset = 0_i64;
    while offset < declared_size {
        let key = upload_part_key(session_id, offset);
        if let Err(error) = storage.delete(&key).await {
            tracing::warn!(%session_id, %key, %error, "removing a staged upload part stalled");
        }
        offset += chunk;
    }
}

/// Best-effort removal of the session row. A failure is logged: the next
/// completion of the same id re-files idempotently until the row goes.
async fn remove_upload_session_row(db: &SurrealDb, session_id: Uuid) {
    let result = crate::assets::writing(|| {
        db.query("DELETE $id")
            .bind(("id", record_id(UPLOAD_SESSION_TABLE, session_id)))
    })
    .await;
    if let Err(error) = result {
        tracing::warn!(%session_id, error = %error, "removing a completed upload session stalled");
    }
}

/// The row as the engine reads it. Separate from [`UploadSession`]
/// because the SDK owns its own `RecordId` and `Datetime`, and the
/// conversion belongs at this seam rather than in every caller.
#[derive(SurrealValue)]
struct UploadSessionRow {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    filename: String,
    kind: String,
    content_type: String,
    visibility: String,
    description: Option<String>,
    slug: Option<String>,
    transcript_quality: Option<String>,
    metadata: Option<Json>,
    derived_from: Option<Json>,
    signing: Option<Json>,
    notarization: Option<Json>,
    declared_size: i64,
    declared_sha256: String,
    chunk_size: i64,
    received_bytes: i64,
    created_at: surrealdb::types::Datetime,
    expires_at: surrealdb::types::Datetime,
}

impl UploadSessionRow {
    /// `None` when a record id is not a native UUID key — a row written
    /// by something that bypassed [`crate::surreal::record_id`].
    fn into_upload_session(self) -> Option<UploadSession> {
        Some(UploadSession {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            filename: self.filename,
            kind: self.kind,
            content_type: self.content_type,
            visibility: self.visibility,
            description: self.description,
            slug: self.slug,
            transcript_quality: self.transcript_quality,
            metadata: self.metadata,
            derived_from: self.derived_from,
            signing: self.signing,
            notarization: self.notarization,
            declared_size: self.declared_size,
            declared_sha256: self.declared_sha256,
            chunk_size: self.chunk_size,
            received_bytes: self.received_bytes,
            created_at: self.created_at.into(),
            expires_at: self.expires_at.into(),
        })
    }
}

/// Errors surfaced by [`ingest_bytes`].
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Asset(#[from] crate::assets::AssetError),
    #[error(transparent)]
    Project(#[from] crate::projects::ProjectStoreError),
    /// The insert reported success but returned no row this module could
    /// read back.
    #[error("writing a document asset returned no usable row")]
    WriteReturnedNothing,
    /// A stable server-generated document identity was reused for different
    /// bytes or different document metadata.
    #[error("idempotent document identity conflicts with an existing asset: {0}")]
    IdempotencyConflict(String),
    /// `args.kind` is not a [`rules::kind::Kind`] valid in
    /// [`rules::kind::Lane::Asset`]. The ingest boundary rejects rather than
    /// silently coercing to `unclassified` — a lane whose whole job is
    /// classification should refuse a bad classification loudly, not file it
    /// under a value the caller never chose.
    #[error("`{0}` is not a valid asset-lane kind")]
    InvalidKind(String),
}

/// Inputs to [`ingest_bytes`]. Held in a struct (rather than a long
/// positional list) so future fields don't break callers.
#[derive(Debug, Clone)]
pub struct IngestArgs<'a> {
    /// Project the document belongs to.
    pub project_id: Uuid,
    /// Inbound channel name — `upload`, `email`, `generated`, or `drive`.
    /// Goes into `assets.source`.
    pub source: &'a str,
    /// Caller-visible filename. Goes into `assets.filename`.
    pub filename: &'a str,
    /// Document classification — one of [`rules::kind::Kind::valid_for`]
    /// [`rules::kind::Lane::Asset`] (`onboarding`, `offboarding`,
    /// `unclassified`, …). Goes into `assets.kind`; [`ingest_bytes`] rejects
    /// any other value.
    pub kind: &'a str,
    /// MIME content type of `bytes`.
    pub content_type: &'a str,
    /// Optional lawyer-view caption; goes into `assets.description`.
    pub description: Option<&'a str>,
    /// Matter-page visibility — `visibility::INTERNAL` or
    /// `visibility::CLIENT`. Every caller states this explicitly (no silent
    /// default at the Rust layer) even though the column itself defaults to
    /// `internal`, so a call site that should be client-visible can't
    /// silently forget to say so.
    pub visibility: &'a str,
    /// A second object-storage key holding the **same bytes**, written
    /// outside content-addressing — a generated PDF's notation key
    /// (`notations/<id>/document.pdf`). Stamped onto the asset row in the
    /// **same transaction** as the insert so every asset row for the bytes
    /// carries it, and a governed expunge deletes every copy (#470). `None`
    /// for the common single-copy ingest.
    pub secondary_storage_key: Option<&'a str>,
}

/// The **document identity** an ingest optionally carries — the asset
/// lane's slug, publication stamp, and free-form detail.
///
/// Separate from [`IngestArgs`] rather than three more fields on it
/// because the two answer different questions. `IngestArgs` describes
/// *these bytes*: where they came from, what they are, who may see them.
/// This describes *which document they are a revision of*, and almost
/// every caller has no answer — an inbound email attachment or a
/// generated PDF is a one-off artifact, not a revision of anything. Those
/// callers use [`ingest_bytes`] and get [`DocumentIdentity::default`],
/// a `NULL` slug.
///
/// A caller that *does* know the document identity goes through
/// [`crate::assets::file_revision`], which enforces the asset lane's
/// write rules rather than writing a row directly.
#[derive(Debug, Clone, Default)]
pub struct DocumentIdentity<'a> {
    /// The lawyer-chosen document identity within the Project, or `None`
    /// for a one-off artifact. Never derived from the filename: a
    /// re-upload named `captable_final_v2.pdf` must not fork a chain, and
    /// two unrelated `agreement.pdf`s must not merge into one.
    pub slug: Option<&'a str>,
    /// RFC 3339 publication stamp, back-datable to a court's file stamp.
    /// Distinct from `received_at`, which every ingest stamps with the
    /// moment the bytes arrived.
    pub published_at: Option<&'a str>,
    /// Free-form per-kind detail — a back-dating reason, a court's entry
    /// sequence. Validators belong in the `rules` crate, never a database
    /// CHECK.
    pub metadata: Option<serde_json::Value>,
    /// JSON `derived_from` relation for generated Project documents such as
    /// OCR transcripts. The shape is `{document_id, version, sha256}`.
    pub derived_from: Option<serde_json::Value>,
    /// Transcript provenance quality; other document kinds leave it unset.
    pub transcript_quality: Option<&'a str>,
    /// How an executed document was signed. Optional on every kind.
    pub signing: Option<Signing>,
    /// The notarial acknowledgments on an executed document, if any.
    pub notarization: Vec<Notarization>,
    /// The exhibit metadata the evidence index reads (LAW-185). Optional on
    /// every kind; validated by the caller before any byte is stored.
    pub evidence: Option<Evidence>,
}

/// How an executed document was signed: in wet ink on paper, or through
/// `DocuSign`. See the glossary's Signing Method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningMethod {
    WetInk,
    Docusign,
}

impl SigningMethod {
    /// Every method, in the order help and errors name them.
    pub const ALL: [Self; 2] = [Self::WetInk, Self::Docusign];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WetInk => "wet_ink",
            Self::Docusign => "docusign",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
    }
}

/// The signing record on an executed document: its [`SigningMethod`] and the
/// calendar date it was signed (`YYYY-MM-DD`). Stored on the `asset` row and
/// carried on its committed pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    pub method: SigningMethod,
    pub signed_on: chrono::NaiveDate,
}

/// How a notarial acknowledgment was taken: before a notary in person, or
/// by remote online notarization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotarizationMethod {
    InPerson,
    RemoteOnline,
}

impl NotarizationMethod {
    /// Every method, in the order help and errors name them.
    pub const ALL: [Self; 2] = [Self::InPerson, Self::RemoteOnline];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InPerson => "in_person",
            Self::RemoteOnline => "remote_online",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
    }
}

/// One notarial acknowledgment on an executed document. A packet may hold
/// several, so a document carries a list. It records where and how the
/// acknowledgment was taken and when the notary's commission expires, and
/// deliberately never the notary's name or commission number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notarization {
    pub method: NotarizationMethod,
    pub acknowledged_on: chrono::NaiveDate,
    /// The two-letter state code where the notary is commissioned, such as `NV`.
    pub state: String,
    pub county: String,
    pub commission_expires: chrono::NaiveDate,
}

/// Why a [`Notarization`] cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotarizationError {
    #[error("notarization state must be a two-letter code such as `NV`, not `{0}`")]
    State(String),
    #[error("notarization county must not be blank")]
    County,
    #[error("the notary's commission must not expire before the acknowledgment date")]
    CommissionExpired,
}

impl Notarization {
    /// Enforce what the field types cannot: a two-letter uppercase state
    /// code, a non-blank county, and a commission still in force on the
    /// acknowledgment date.
    ///
    /// # Errors
    /// The first [`NotarizationError`].
    pub fn validate(&self) -> Result<(), NotarizationError> {
        if self.state.len() != 2 || !self.state.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(NotarizationError::State(self.state.clone()));
        }
        if self.county.trim().is_empty() {
            return Err(NotarizationError::County);
        }
        if self.commission_expires < self.acknowledged_on {
            return Err(NotarizationError::CommissionExpired);
        }
        Ok(())
    }
}

/// How confidently an exhibit is tied to the claim it bears on, in the
/// evidence index's closed vocabulary (LAW-185). `exact` is a link a lawyer
/// verified; `inferred` is a link filed by routing with a human expected to
/// confirm it; `unknown` is everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Exact,
    Inferred,
    Unknown,
}

impl EvidenceConfidence {
    /// Every confidence, in the order help and errors name them.
    pub const ALL: [Self; 3] = [Self::Exact, Self::Inferred, Self::Unknown];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|confidence| confidence.as_str() == value)
    }
}

/// The exhibit metadata the matter page's evidence index reads (LAW-185):
/// the date the exhibit itself bears, where it came from, how confidently it
/// is tied to what it supports, and the claim references it bears on. Stored
/// on the `asset` row and carried on its committed pointer. Every field is
/// optional — an exhibit with none recorded still indexes, showing its gaps.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The date the exhibit bears, when it has one — distinct from
    /// `published_at` (a court's file stamp) and `received_at` (when the
    /// bytes arrived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dated_at: Option<chrono::NaiveDate>,
    /// Where the exhibit came from, as the lawyer knows it — an email, a
    /// message thread, a download. Never a `source` channel: the asset's own
    /// `source` column already records the upload lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// How confidently the exhibit is tied to what it supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<EvidenceConfidence>,
    /// The claim references the exhibit bears on, such as `¶ 12` or
    /// `Count II — Conversion`. An exhibit bearing on nothing reads back as
    /// an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supports: Vec<String>,
}

/// Why an [`Evidence`] block cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceError {
    #[error("evidence source must not be blank")]
    Source,
    #[error("evidence claim reference {0} must not be blank")]
    Claim(usize),
}

impl Evidence {
    /// Enforce what the field types cannot: a non-blank source, and
    /// non-blank claim references.
    ///
    /// # Errors
    /// The first [`EvidenceError`].
    pub fn validate(&self) -> Result<(), EvidenceError> {
        if self
            .source
            .as_deref()
            .is_some_and(|source| source.trim().is_empty())
        {
            return Err(EvidenceError::Source);
        }
        for (index, claim) in self.supports.iter().enumerate() {
            if claim.trim().is_empty() {
                return Err(EvidenceError::Claim(index));
            }
        }
        Ok(())
    }
}

/// The exact source document revision a generated transcript was produced from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DerivedFrom {
    pub document_id: uuid::Uuid,
    pub version: usize,
    pub sha256: String,
}

/// Inbound-mail provenance for one filed attachment (ENG-517) — the sender,
/// message id, received timestamp, and subject of the
/// `email_conversation_message` the bytes came from. A hand-download loses
/// all four, which is exactly the gap `navigator site mail file` closes.
///
/// Deliberately not a field on [`IngestArgs`] or [`DocumentIdentity`]: those
/// are constructed at roughly two dozen call sites across the workspace, and
/// only mail filing's one server-side handler ever has this to give. A
/// separate, `Default`-derived struct threaded through
/// [`ingest_bytes_with_mail_provenance`] keeps every existing call site
/// untouched.
#[derive(Debug, Clone, Copy, Default)]
pub struct MailProvenance<'a> {
    pub message_id: &'a str,
    pub sender: &'a str,
    pub received_at: &'a str,
    pub subject: &'a str,
}

/// What [`ingest_bytes`] writes, returned for the caller to log /
/// reference / show in a UI.
#[derive(Debug, Clone)]
pub struct IngestedDocument {
    /// The `assets` row id for this document.
    pub asset_id: Uuid,
    /// Project-scoped content-addressed object key written for this document.
    pub storage_key: String,
    pub sha256_hex: String,
    pub byte_size: i64,
    /// `true` when this Project already stored the bytes under another asset,
    /// so no new storage write happened.
    pub reused: bool,
}

/// Ingest one artifact: write the bytes (if new), insert the document
/// `asset` row, return the id.
///
/// # Errors
/// [`IngestError`] on a storage or database failure.
pub async fn ingest_bytes(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    ingest_bytes_as(db, storage, args, &DocumentIdentity::default(), bytes).await
}

/// Ingest one artifact exactly once for a `(Project, filename, content)`
/// identity. Restate replay may run the caller again after the asset row was
/// committed; returning that row prevents a second internal document record
/// while retaining the ordinary content-addressed storage semantics.
pub async fn ingest_bytes_exactly_once(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    let sha_hex = crate::assets::sha256_hex(bytes);
    if let Some(existing) =
        crate::assets::find_filed_copy(db, args.project_id, args.filename, &sha_hex).await?
    {
        return Ok(IngestedDocument {
            asset_id: existing.id,
            storage_key: existing.storage_key,
            sha256_hex: existing.sha256_hex,
            byte_size: existing.byte_size,
            reused: true,
        });
    }
    ingest_bytes(db, storage, args, bytes).await
}

/// Ingest one server-generated document under a caller-supplied stable key.
/// The key must be namespaced to the document purpose and include the Project
/// and canonical filename. A deterministic Surreal record id elects one
/// writer when requests race; concurrent losers return the exact row written
/// by the winner, but only when bytes and document metadata match.
///
/// Object storage is still written before the asset row, so no row can point
/// at bytes that failed to persist. A losing concurrent put is content
/// addressed and harmless; its row insert resolves to the winner's record.
///
/// # Errors
/// [`IngestError`] on storage/database failure or when a stable key is reused
/// for different bytes or metadata.
pub async fn ingest_bytes_with_idempotency_key(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    idempotency_key: &str,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    let expected_sha = sha256_hex(bytes);
    if let Some(existing) =
        crate::assets::find_by_project_and_filename(db, args.project_id, args.filename).await?
    {
        return existing_document(existing, args, &expected_sha);
    }

    let asset_id = stable_asset_id(args.project_id, args.filename, idempotency_key);
    match ingest_bytes_as_inner(
        db,
        storage,
        args,
        &DocumentIdentity::default(),
        None,
        Some(asset_id),
        bytes,
    )
    .await
    {
        Ok(document) => Ok(document),
        Err(error) => {
            if let Some(existing) = crate::assets::find_by_id(db, asset_id).await? {
                return existing_document(existing, args, &expected_sha);
            }
            Err(error)
        }
    }
}

fn existing_document(
    existing: crate::assets::Asset,
    args: &IngestArgs<'_>,
    expected_sha: &str,
) -> Result<IngestedDocument, IngestError> {
    if existing.project_id != Some(args.project_id)
        || existing.filename.as_deref() != Some(args.filename)
        || existing.kind.as_deref() != Some(args.kind)
        || existing.source.as_deref() != Some(args.source)
        || existing.content_type != args.content_type
        || existing.description.as_deref() != args.description
        || existing.secondary_storage_key != args.secondary_storage_key.map(str::to_string)
        || existing.visibility != args.visibility
        || existing.sha256_hex != expected_sha
    {
        return Err(IngestError::IdempotencyConflict(args.filename.to_string()));
    }
    Ok(IngestedDocument {
        asset_id: existing.id,
        storage_key: existing.storage_key,
        sha256_hex: existing.sha256_hex,
        byte_size: existing.byte_size,
        reused: true,
    })
}

fn stable_asset_id(project_id: Uuid, filename: &str, idempotency_key: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(b"navigator/document-idempotency/v1\0");
    digest.update(project_id.as_bytes());
    for part in [filename.as_bytes(), idempotency_key.as_bytes()] {
        digest.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(part);
    }
    let hash = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // Version 8 marks this as a custom SHA-256-derived identity, not an
    // RFC 4122 v5 UUID (which is defined over SHA-1).
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// [`ingest_bytes`], filing the bytes as a revision of the document named
/// by `identity`.
///
/// The raw write. It does **not** enforce the asset lane's rules — that
/// is [`crate::assets::file_revision`]'s job, and slugged callers should
/// go through it so no call site reimplements the no-op and immutability
/// checks and drifts.
///
/// # Errors
/// Propagates database and storage errors.
pub async fn ingest_bytes_as(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    ingest_bytes_as_inner(db, storage, args, identity, None, None, bytes).await
}

/// [`ingest_bytes_as`], additionally stamping [`MailProvenance`] onto the
/// inserted row (ENG-517). The one caller is `navigator site mail file`'s
/// server-side handler; every other ingest lane goes through
/// [`ingest_bytes_as`] and leaves the four provenance columns unset.
///
/// # Errors
/// Propagates database and storage errors.
pub async fn ingest_bytes_with_mail_provenance(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    provenance: &MailProvenance<'_>,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    ingest_bytes_as_inner(db, storage, args, identity, Some(provenance), None, bytes).await
}

async fn ingest_bytes_as_inner(
    db: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    provenance: Option<&MailProvenance<'_>>,
    asset_id: Option<Uuid>,
    bytes: &[u8],
) -> Result<IngestedDocument, IngestError> {
    if !rules::kind::Kind::parse(args.kind).is_some_and(|k| k.valid_for(rules::kind::Lane::Asset)) {
        return Err(IngestError::InvalidKind(args.kind.to_string()));
    }

    let sha_hex = sha256_hex(bytes);
    let byte_size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    let project = crate::projects::find_by_id(db, args.project_id)
        .await?
        .ok_or(crate::projects::ProjectStoreError::NoSuchProject(
            args.project_id,
        ))?;
    let storage_key = format!(
        "{}/{sha_hex}",
        cloud::workspace::documents_prefix(&project.code)
    );

    // Storage dedup, scoped to this matter: if an asset **on this project**
    // already references this content, the object is stored; otherwise write
    // it once. Write before inserting the row so a crash mid-ingest never
    // leaves an `asset` row pointing at a key that doesn't exist — the
    // ordering is what carries that, not a transaction (see the module doc).
    //
    // The `project_id` filter is the whole point: deduping across matters
    // made one matter's governed expunge destroy another's document. Two
    // matters holding the same exhibit is ordinary; a sealing order on one
    // silently emptying the other is not. Bytes are cheap, cross-matter
    // coupling is not.
    let reused = project_holds_content(db, args.project_id, &sha_hex).await?;
    if !reused {
        storage.put(&storage_key, bytes, args.content_type).await?;
    }

    let asset_id = insert_asset_row(
        db,
        args,
        identity,
        provenance,
        StoredDocumentBytes {
            storage_key: &storage_key,
            sha256_hex: &sha_hex,
            byte_size,
        },
        asset_id,
    )
    .await?;

    Ok(IngestedDocument {
        asset_id,
        storage_key,
        sha256_hex: sha_hex,
        byte_size,
        reused,
    })
}

/// Whether this matter already holds an asset referencing `sha_hex`.
async fn project_holds_content(
    db: &SurrealDb,
    project_id: Uuid,
    sha_hex: &str,
) -> Result<bool, IngestError> {
    let mut response = db
        .query(format!(
            "SELECT VALUE id FROM {TABLE} \
             WHERE sha256_hex = $sha AND project_id = $project LIMIT 1"
        ))
        .bind(("sha", sha_hex.to_string()))
        .bind((
            "project",
            record_id(crate::projects::PROJECT_TABLE, project_id),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let found: Option<surrealdb::types::RecordId> = response.take(0)?;
    Ok(found.is_some())
}

/// The content hash every document is stored and deduped under.
///
/// Public so a caller that needs to ask "does this project already hold
/// these bytes?" hashes them exactly the way ingest does — a second
/// implementation would drift and silently stop matching. Delegates to
/// [`crate::assets::sha256_hex`] so both lanes share one digest.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    asset_sha256_hex(bytes)
}

/// The docket-record attestation (LAW-24, option 1): does an independently
/// fetched copy of a filed document hash to the same bytes Navigator
/// stored?
///
/// This is the real authenticity check available for a court PDF — these
/// carry no `/ByteRange`, `/Sig`, or `SigFlags`, so there is no signature to
/// verify. Comparing against a second, independently obtained copy is
/// possible instead: fetching that copy is the caller's job (the source is
/// the [`crate::cases::Case`]'s `forum`/`docket_number` and the
/// [`crate::cases::DocketEntry`]'s `entry_number`), and this function does
/// only the comparison, so the one hash algorithm both sides agree on
/// cannot drift between the stored-document path and this one.
#[must_use]
pub fn verify_independent_copy(recorded_sha256_hex: &str, independently_fetched: &[u8]) -> bool {
    sha256_hex(independently_fetched) == recorded_sha256_hex
}

struct StoredDocumentBytes<'a> {
    storage_key: &'a str,
    sha256_hex: &'a str,
    byte_size: i64,
}

async fn insert_asset_row(
    db: &SurrealDb,
    args: &IngestArgs<'_>,
    identity: &DocumentIdentity<'_>,
    provenance: Option<&MailProvenance<'_>>,
    bytes: StoredDocumentBytes<'_>,
    asset_id: Option<Uuid>,
) -> Result<Uuid, IngestError> {
    let id = asset_id.unwrap_or_else(Uuid::now_v7);
    let mut response = crate::assets::writing(|| {
        db.query(format!(
            "CREATE $id SET \
             storage_key = $storage_key, \
             secondary_storage_key = $secondary_storage_key, \
             content_type = $content_type, \
             byte_size = $byte_size, \
             sha256_hex = $sha256_hex, \
             project_id = $project_id, \
             filename = $filename, \
             kind = $kind, \
             source = $source, \
             received_at = $received_at, \
             description = $description, \
             visibility = $visibility, \
             slug = $slug, \
             published_at = $published_at, \
             source_message_id = $source_message_id, \
             source_sender = $source_sender, \
             source_received_at = $source_received_at, \
             source_subject = $source_subject, \
             metadata = $metadata, \
             derived_from = $derived_from, \
             transcript_quality = $transcript_quality, \
             signing = $signing, \
             notarization = $notarization \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("storage_key", bytes.storage_key.to_string()))
        .bind((
            "secondary_storage_key",
            args.secondary_storage_key.map(String::from),
        ))
        .bind(("content_type", args.content_type.to_string()))
        .bind(("byte_size", bytes.byte_size))
        .bind(("sha256_hex", bytes.sha256_hex.to_string()))
        .bind((
            "project_id",
            record_id(crate::projects::PROJECT_TABLE, args.project_id),
        ))
        .bind(("filename", Some(args.filename.to_string())))
        .bind(("kind", Some(args.kind.to_string())))
        .bind(("source", Some(args.source.to_string())))
        .bind(("received_at", Some(Utc::now().to_rfc3339())))
        .bind(("description", args.description.map(String::from)))
        .bind(("visibility", args.visibility.to_string()))
        .bind(("slug", identity.slug.map(String::from)))
        .bind(("published_at", identity.published_at.map(String::from)))
        .bind((
            "source_message_id",
            provenance.map(|p| p.message_id.to_string()),
        ))
        .bind(("source_sender", provenance.map(|p| p.sender.to_string())))
        .bind((
            "source_received_at",
            provenance.map(|p| p.received_at.to_string()),
        ))
        .bind(("source_subject", provenance.map(|p| p.subject.to_string())))
        .bind(("metadata", identity.metadata.clone()))
        .bind(("derived_from", identity.derived_from.clone()))
        .bind((
            "transcript_quality",
            identity.transcript_quality.map(String::from),
        ))
        .bind((
            "signing",
            identity
                .signing
                .and_then(|signing| serde_json::to_value(signing).ok()),
        ))
        .bind((
            "notarization",
            (!identity.notarization.is_empty())
                .then(|| serde_json::to_value(&identity.notarization).ok())
                .flatten(),
        ))
        .bind((
            "evidence",
            identity
                .evidence
                .as_ref()
                .and_then(|evidence| serde_json::to_value(evidence).ok()),
        ))
    })
    .await?;

    let written: Option<surrealdb::types::RecordId> = response.take((0, "id"))?;
    let id = written
        .as_ref()
        .and_then(crate::surreal::record_uuid)
        .ok_or(IngestError::WriteReturnedNothing)?;
    crate::assets::mirror_writes(db, &[id]).await;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_asset_ids_include_project_filename_and_document_purpose_key() {
        let project_a = Uuid::from_u128(1);
        let project_b = Uuid::from_u128(2);
        let id = stable_asset_id(project_a, "letter.pdf", "pitch-offboarding/v1");
        assert_eq!(id.get_version_num(), 8);
        assert_ne!(
            id,
            stable_asset_id(project_b, "letter.pdf", "pitch-offboarding/v1")
        );
        assert_ne!(
            id,
            stable_asset_id(project_a, "other.pdf", "pitch-offboarding/v1")
        );
        assert_ne!(
            id,
            stable_asset_id(project_a, "letter.pdf", "other-purpose/v1")
        );
    }
    use crate::assets::find_by_id;
    use crate::surreal::test_support::mem;
    use cloud::FsStorage;

    async fn fixtures() -> (SurrealDb, Arc<dyn StorageService>, tempfile::TempDir, Uuid) {
        let db = mem().await;
        let tmp = tempfile::tempdir().unwrap();
        let storage: Arc<dyn StorageService> =
            Arc::new(FsStorage::new(tmp.path().to_path_buf()).await.unwrap());
        let project_id = crate::test_support::seed_project_surreal(&db, "Test Matter").await;
        (db, storage, tmp, project_id)
    }

    #[test]
    fn the_request_cap_covers_a_maximum_document_as_base64_plus_its_json() {
        // base64 spends four characters on every three bytes, rounding the
        // final partial group up. Truncating that division instead spends
        // part of the JSON allowance on the encoding, so a maximum-size
        // document carrying a `description` or `metadata` would be refused
        // by the framework's body cap rather than by the door's own size
        // check — the raw buffering error ENG-602 set out to replace.
        use base64::Engine as _;

        let engine = base64::engine::general_purpose::STANDARD;
        for len in 0..=9usize {
            assert_eq!(len.div_ceil(3) * 4, engine.encode(vec![0u8; len]).len());
        }

        assert!(
            MAX_DOCUMENT_UPLOAD_REQUEST_BYTES
                >= MAX_DOCUMENT_UPLOAD_BYTES.div_ceil(3) * 4 + MAX_DOCUMENT_UPLOAD_JSON_BYTES
        );
    }

    #[test]
    fn transcript_documents_may_carry_court_audio_past_the_general_limit() {
        assert_eq!(max_document_upload_bytes("memo"), MAX_DOCUMENT_UPLOAD_BYTES);
        assert_eq!(
            max_document_upload_bytes("transcript"),
            MAX_MEDIA_DOCUMENT_UPLOAD_BYTES
        );
        // A two-hour hearing recording of 59,931,843 bytes fits.
        assert!(59_931_843 <= max_document_upload_bytes("transcript"));
        assert!(
            MAX_DOCUMENT_UPLOAD_REQUEST_BYTES
                >= MAX_MEDIA_DOCUMENT_UPLOAD_BYTES.div_ceil(3) * 4 + MAX_DOCUMENT_UPLOAD_JSON_BYTES
        );
    }

    #[test]
    fn oversize_message_names_the_limit_for_the_kind_and_where_larger_files_go() {
        let message = document_upload_size_message_for_kind("memo", 30_000_000);
        assert!(message.contains(&MAX_DOCUMENT_UPLOAD_BYTES.to_string()));
        assert!(message.contains("30000000"));
        assert!(message.contains("--kind transcript"));
        let message = document_upload_size_message_for_kind("transcript", 200_000_000);
        assert!(message.contains(&MAX_MEDIA_DOCUMENT_UPLOAD_BYTES.to_string()));
    }

    #[test]
    fn verify_independent_copy_compares_by_hash_not_by_bytes_identity() {
        let recorded = sha256_hex(b"the filed document");
        assert!(verify_independent_copy(&recorded, b"the filed document"));
        assert!(!verify_independent_copy(&recorded, b"a tampered document"));
    }

    #[test]
    fn a_document_slug_keeps_the_uploaded_filename_extension() {
        assert!(validate_document_slug("complaint.pdf", "dkt-001-complaint.pdf").is_ok());
        assert!(validate_document_slug("complaint.PDF", "dkt-001-complaint.pdf").is_ok());

        let error = validate_document_slug("complaint.pdf", "dkt-001-complaint").unwrap_err();
        assert_eq!(
            error.to_string(),
            "document slug `dkt-001-complaint` must carry the source extension from `complaint.pdf`; use `dkt-001-complaint.pdf` so it can be represented as a Pointer"
        );
        assert!(matches!(
            error,
            DocumentSlugError::MissingOrMismatchedExtension { .. }
        ));
        assert!(validate_document_slug("complaint.pdf", "dkt-001-complaint.txt").is_err());
        assert!(validate_document_slug("complaint", "dkt-001-complaint").is_err());
    }

    #[tokio::test]
    async fn ingest_writes_a_document_asset_with_provenance() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "retainer.pdf",
            kind: "onboarding",
            content_type: "application/pdf",
            description: Some("client-signed retainer"),
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };

        let out = ingest_bytes(&db, &storage, &args, b"hello world")
            .await
            .unwrap();

        assert!(!out.reused);
        assert_eq!(out.byte_size, 11);
        // sha256("hello world") =
        //   b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9
        assert_eq!(
            out.sha256_hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );

        let a = find_by_id(&db, out.asset_id)
            .await
            .unwrap()
            .expect("asset row");
        assert_eq!(a.content_type, "application/pdf");
        assert_eq!(a.byte_size, 11);
        let project = crate::projects::find_by_id(&db, project_id)
            .await
            .unwrap()
            .expect("project row");
        assert_eq!(
            a.storage_key,
            format!("projects/{}/documents/{}", project.code, out.sha256_hex)
        );
        assert_eq!(a.filename.as_deref(), Some("retainer.pdf"));
        assert_eq!(a.kind.as_deref(), Some("onboarding"));
        assert_eq!(a.project_id, Some(project_id));
        assert_eq!(a.source.as_deref(), Some("upload"));
        assert_eq!(a.description.as_deref(), Some("client-signed retainer"));
        assert!(
            a.received_at.is_some(),
            "received_at must be stamped on insert"
        );
        assert_eq!(a.visibility, visibility::INTERNAL);

        let stored = storage.get(&a.storage_key).await.unwrap();
        assert_eq!(stored.bytes, b"hello world");
    }

    #[tokio::test]
    async fn ingest_refuses_an_unknown_project_before_writing() {
        let (db, storage, _tmp, _project_id) = fixtures().await;
        let missing_project = Uuid::now_v7();
        let args = IngestArgs {
            project_id: missing_project,
            source: source::UPLOAD,
            filename: "retainer.pdf",
            kind: "onboarding",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };

        let error = ingest_bytes(&db, &storage, &args, b"hello world")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            IngestError::Project(crate::projects::ProjectStoreError::NoSuchProject(id)) if id == missing_project
        ));
    }

    #[tokio::test]
    async fn ingest_dedupes_storage_but_keeps_distinct_asset_rows() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let bytes = b"same bytes";
        let mk = |fname: &'static str| IngestArgs {
            project_id,
            source: "upload",
            filename: fname,
            kind: "unclassified",
            content_type: "text/plain",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };

        let first = ingest_bytes(&db, &storage, &mk("a.txt"), bytes)
            .await
            .unwrap();
        assert!(!first.reused);

        let second = ingest_bytes(&db, &storage, &mk("b.txt"), bytes)
            .await
            .unwrap();
        assert!(second.reused, "the content-addressed object is reused");
        assert_ne!(
            second.asset_id, first.asset_id,
            "each document ingest is a distinct asset row, even sharing bytes"
        );

        // Two asset rows on this matter, one shared storage key. Scoped to
        // the project rather than counted table-wide: the cucumber suite
        // shares one engine across concurrent scenarios.
        let assets = crate::assets::for_project(&db, project_id).await.unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].storage_key, assets[1].storage_key);
    }

    #[tokio::test]
    async fn exact_once_ingest_reuses_the_committed_document_row_on_replay() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "generated",
            filename: "reviewed-contract.docx",
            kind: "inbound_contract",
            content_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            description: Some("Attorney-reviewed Word contract"),
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        let first = ingest_bytes_exactly_once(&db, &storage, &args, b"reviewed bytes")
            .await
            .unwrap();
        let second = ingest_bytes_exactly_once(&db, &storage, &args, b"reviewed bytes")
            .await
            .unwrap();
        assert_eq!(first.asset_id, second.asset_id);
        assert!(!first.reused);
        assert!(second.reused);
    }

    #[tokio::test]
    async fn keyed_ingest_rejects_changed_bytes_without_replacing_the_filed_document() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: source::GENERATED,
            filename: "pitch-offboarding-letter.pdf",
            kind: "offboarding",
            content_type: "application/pdf",
            description: Some("Generated pitch offboarding letter"),
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        let first = ingest_bytes_with_idempotency_key(
            &db,
            &storage,
            &args,
            "pitch-offboarding/v1",
            b"first letter",
        )
        .await
        .unwrap();
        let error = ingest_bytes_with_idempotency_key(
            &db,
            &storage,
            &args,
            "pitch-offboarding/v1",
            b"different letter",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, IngestError::IdempotencyConflict(_)));

        let saved = find_by_id(&db, first.asset_id).await.unwrap().unwrap();
        assert_eq!(saved.sha256_hex, sha256_hex(b"first letter"));
        assert_eq!(
            storage.get(&saved.storage_key).await.unwrap().bytes,
            b"first letter"
        );
    }

    #[tokio::test]
    async fn ingest_does_not_dedupe_across_matters() {
        // Dedup is scoped to one matter. Two matters that happen to receive
        // identical bytes each get their own storage write.
        //
        // Global dedup made one matter's governed expunge destroy another's
        // document. Two clients sharing a common exhibit is ordinary; one
        // client's sealing order silently emptying an unrelated matter is not.
        let (db, storage, _tmp, first) = fixtures().await;
        let second = crate::test_support::seed_project_surreal(&db, "Second Matter").await;
        let bytes = b"an exhibit filed on two matters";
        let mk = |project_id: Uuid| IngestArgs {
            project_id,
            source: "upload",
            filename: "exhibit.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };

        let a = ingest_bytes(&db, &storage, &mk(first), bytes)
            .await
            .unwrap();
        assert!(!a.reused);

        let b = ingest_bytes(&db, &storage, &mk(second), bytes)
            .await
            .unwrap();
        assert!(
            !b.reused,
            "a second matter must own its copy, not reuse the first matter's object"
        );

        // The same hash is stored at two Project-scoped keys. Each matter owns
        // its copy, which is what `reused: false` records.
        assert_eq!(a.sha256_hex, b.sha256_hex);
        assert_ne!(a.asset_id, b.asset_id);
        let first_asset = find_by_id(&db, a.asset_id)
            .await
            .unwrap()
            .expect("first asset");
        let second_asset = find_by_id(&db, b.asset_id)
            .await
            .unwrap()
            .expect("second asset");
        assert_ne!(first_asset.storage_key, second_asset.storage_key);
        assert!(first_asset.storage_key.ends_with(&a.sha256_hex));
        assert!(second_asset.storage_key.ends_with(&b.sha256_hex));
    }

    #[tokio::test]
    async fn ingest_different_bytes_produces_different_assets() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let mk = |fname: &'static str| IngestArgs {
            project_id,
            source: "upload",
            filename: fname,
            kind: "unclassified",
            content_type: "text/plain",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        let a = ingest_bytes(&db, &storage, &mk("a.txt"), b"alpha")
            .await
            .unwrap();
        let b = ingest_bytes(&db, &storage, &mk("b.txt"), b"bravo")
            .await
            .unwrap();
        assert_ne!(a.asset_id, b.asset_id);
        assert_ne!(a.sha256_hex, b.sha256_hex);
    }

    #[tokio::test]
    async fn ingest_stamps_the_secondary_key_with_the_row() {
        // #470: a generated PDF's notation-key copy is recorded on the asset
        // row by the SAME statement that creates it, so every asset row for
        // the bytes carries it and a governed expunge can delete every copy —
        // no window where a committed row is missing its second key.
        let (db, storage, _tmp, project_id) = fixtures().await;
        let notation_key = "notations/abc/document.pdf";
        let args = IngestArgs {
            project_id,
            source: source::GENERATED,
            filename: "document.pdf",
            kind: "onboarding",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: Some(notation_key),
            visibility: visibility::INTERNAL,
        };
        let ingested = ingest_bytes(&db, &storage, &args, b"%PDF-1.7 body")
            .await
            .unwrap();

        let row = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert_eq!(row.secondary_storage_key.as_deref(), Some(notation_key));
        let project = crate::projects::find_by_id(&db, project_id)
            .await
            .unwrap()
            .expect("project row");
        assert_eq!(
            row.storage_key,
            format!(
                "projects/{}/documents/{}",
                project.code, ingested.sha256_hex
            )
        );
    }

    #[tokio::test]
    async fn ingest_leaves_the_secondary_key_unset_by_default() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "a.txt",
            kind: "unclassified",
            content_type: "text/plain",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        let ingested = ingest_bytes(&db, &storage, &args, b"plain").await.unwrap();
        let row = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert_eq!(row.secondary_storage_key, None);
    }

    #[tokio::test]
    async fn ingest_stamps_the_caller_stated_visibility() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "client-visible.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::CLIENT,
        };
        let ingested = ingest_bytes(&db, &storage, &args, b"client bytes")
            .await
            .unwrap();
        let row = find_by_id(&db, ingested.asset_id).await.unwrap().unwrap();
        assert_eq!(row.visibility, visibility::CLIENT);
    }

    /// The schema ASSERT is what closes the visibility vocabulary.
    /// A typo'd visibility would drop a document out of the client's "Your
    /// documents" silently rather than fail, so it must fail at write time.
    #[tokio::test]
    async fn an_unknown_visibility_is_refused_by_the_schema() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "typo.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: "clientt",
        };
        assert!(
            ingest_bytes(&db, &storage, &args, b"bytes").await.is_err(),
            "the engine accepted visibility `clientt`"
        );
    }

    /// The ingest boundary is the asset lane's write gate: a `kind` outside
    /// `rules::kind::Kind::valid_for(Lane::Asset)` must be refused before any
    /// bytes are written or any row is inserted, not silently coerced to
    /// `unclassified`.
    #[tokio::test]
    async fn ingest_bytes_rejects_a_kind_outside_the_asset_lane() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "typo.pdf",
            kind: "bogus",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        assert!(
            matches!(
                ingest_bytes(&db, &storage, &args, b"bytes").await,
                Err(IngestError::InvalidKind(k)) if k == "bogus"
            ),
            "the engine accepted kind `bogus`"
        );
    }

    /// A template-lane-only kind (a matter dashboard, a content page) is not
    /// a document classification either — the same lane split
    /// `rules::kind::Kind::valid_for` enforces on the template side must hold
    /// on the asset side.
    #[tokio::test]
    async fn ingest_bytes_rejects_a_template_lane_only_kind() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let args = IngestArgs {
            project_id,
            source: "upload",
            filename: "not-a-workshop.pdf",
            kind: "workshop",
            content_type: "application/pdf",
            description: None,
            secondary_storage_key: None,
            visibility: visibility::INTERNAL,
        };
        assert!(
            ingest_bytes(&db, &storage, &args, b"bytes").await.is_err(),
            "the engine accepted the template-lane-only kind `workshop`"
        );
    }

    // ---- streaming upload lane (LAW-172) ----

    /// A session mid-flight: four-byte chunks, a 100-byte file, 40 received.
    fn live_session(chunk_size: i64, declared: i64, received: i64) -> UploadSession {
        UploadSession {
            id: Uuid::now_v7(),
            project_id: Uuid::now_v7(),
            filename: "hearing.transcript.mp3".into(),
            kind: "transcript".into(),
            content_type: "audio/mpeg".into(),
            visibility: visibility::INTERNAL.into(),
            description: None,
            slug: None,
            transcript_quality: Some("machine".into()),
            metadata: None,
            derived_from: None,
            signing: None,
            notarization: None,
            declared_size: declared,
            declared_sha256: "0".repeat(64),
            chunk_size,
            received_bytes: received,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
        }
    }

    fn stream_session_args<'a>(
        project_id: Uuid,
        size: usize,
        sha: &'a str,
    ) -> UploadSessionArgs<'a> {
        UploadSessionArgs {
            project_id,
            filename: "hearing.transcript.mp3",
            kind: "transcript",
            content_type: "audio/mpeg",
            visibility: visibility::INTERNAL,
            description: None,
            slug: None,
            transcript_quality: Some("machine"),
            metadata: None,
            derived_from: None,
            signing: None,
            notarization: None,
            declared_size: size,
            declared_sha256: sha,
        }
    }

    #[test]
    fn chunk_math_leaves_the_remainder_for_the_last_append() {
        let chunk = i64::try_from(STREAMING_UPLOAD_CHUNK_SIZE).unwrap();
        // A file smaller than one chunk uploads in a single short chunk.
        assert_eq!(next_chunk_len(100, 0, chunk), Some(100));
        assert_eq!(next_chunk_len(100, 40, chunk), Some(60));
        assert_eq!(
            next_chunk_len(100, 100 - 20, chunk),
            Some(20),
            "the last chunk is the short remainder, not a full chunk"
        );
        assert_eq!(next_chunk_len(100, 100, chunk), None);
        // A multi-chunk file takes full chunks until the remainder.
        let total = 2 * chunk + 20;
        assert_eq!(next_chunk_len(total, 0, chunk), Some(chunk));
        assert_eq!(next_chunk_len(total, chunk, chunk), Some(chunk));
        assert_eq!(next_chunk_len(total, 2 * chunk, chunk), Some(20));
        assert_eq!(next_chunk_len(total, total, chunk), None);
    }

    #[test]
    fn appends_are_canonical_and_exactly_what_the_offset_names() {
        let session = live_session(40, 100, 0);
        assert_eq!(check_append(&session, Utc::now(), 40).unwrap(), 40);
        // A short or long chunk — including an empty one — is refused with
        // the offset and the expected length, which is the retry contract:
        // the client re-syncs from `received_bytes` instead of guessing.
        assert!(matches!(
            check_append(&session, Utc::now(), 0),
            Err(UploadSessionError::ChunkLength {
                offset: 0,
                expected: 40,
                actual: 0
            })
        ));
        assert!(matches!(
            check_append(&session, Utc::now(), 41),
            Err(UploadSessionError::ChunkLength {
                offset: 0,
                expected: 40,
                actual: 41
            })
        ));
        // An overrun past the declared size is a wrong length too: the last
        // chunk must close the file exactly.
        let late = live_session(40, 100, 80);
        assert!(matches!(
            check_append(&late, Utc::now(), 40),
            Err(UploadSessionError::ChunkLength {
                offset: 80,
                expected: 20,
                actual: 40
            })
        ));
    }

    #[test]
    fn a_complete_or_expired_session_accepts_nothing() {
        let done = live_session(40, 100, 100);
        assert!(matches!(
            check_append(&done, Utc::now(), 20),
            Err(UploadSessionError::AlreadyComplete)
        ));
        let mut expired = live_session(40, 100, 0);
        expired.expires_at = Utc::now() - chrono::Duration::hours(1);
        assert!(matches!(
            check_append(&expired, Utc::now(), 40),
            Err(UploadSessionError::Expired(_))
        ));
    }

    #[test]
    fn the_lane_cap_message_names_the_cap() {
        let message = streaming_upload_size_message(MAX_STREAMING_UPLOAD_BYTES + 1);
        assert!(message.contains(&format!("{}", MAX_STREAMING_UPLOAD_BYTES)));
    }

    #[tokio::test]
    async fn the_streaming_lane_refuses_sizes_at_its_boundaries() {
        let (db, _storage, _tmp, project_id) = fixtures().await;
        let sha = crate::assets::sha256_hex(b"hearing");
        // Zero bytes is not an upload.
        assert!(
            create_upload_session(&db, &stream_session_args(project_id, 0, &sha))
                .await
                .is_err(),
            "the lane accepted a zero-byte upload"
        );
        // The cap itself is accepted: create writes a row, no bytes.
        let at_cap = create_upload_session(
            &db,
            &stream_session_args(project_id, MAX_STREAMING_UPLOAD_BYTES, &sha),
        )
        .await
        .unwrap();
        assert_eq!(
            at_cap.declared_size,
            i64::try_from(MAX_STREAMING_UPLOAD_BYTES).unwrap()
        );
        // One byte past it is refused.
        assert!(
            create_upload_session(
                &db,
                &stream_session_args(project_id, MAX_STREAMING_UPLOAD_BYTES + 1, &sha)
            )
            .await
            .is_err(),
            "the lane accepted an upload past its cap"
        );
        // A malformed digest is refused before any row is written.
        assert!(
            create_upload_session(&db, &stream_session_args(project_id, 64, "nothex"))
                .await
                .is_err(),
            "the lane accepted a malformed sha256"
        );
        // A kind the asset lane does not take is refused.
        let mut wrong_kind = stream_session_args(project_id, 64, &sha);
        wrong_kind.kind = "bogus";
        assert!(
            create_upload_session(&db, &wrong_kind).await.is_err(),
            "the lane accepted kind `bogus`"
        );
        // So is a visibility outside the matter-page split.
        let mut wrong_visibility = stream_session_args(project_id, 64, &sha);
        wrong_visibility.visibility = "internal-quiet";
        assert!(
            create_upload_session(&db, &wrong_visibility).await.is_err(),
            "the lane accepted a visibility outside `internal`/`client`"
        );
    }

    #[tokio::test]
    async fn a_session_resumes_from_its_received_bytes_and_files_the_original_bytes() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let chunk = STREAMING_UPLOAD_CHUNK_SIZE;
        let short = 4 * 1024 * 1024;
        let size = chunk * 2 + short; // three parts: full, full, short remainder
        let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let sha = sha256_hex(&bytes);
        let session = create_upload_session(&db, &stream_session_args(project_id, size, &sha))
            .await
            .unwrap();
        assert_eq!(session.chunk_size, i64::try_from(chunk).unwrap());

        // First chunk lands.
        let after_first =
            append_upload_chunk(&db, &storage, project_id, session.id, &bytes[..chunk])
                .await
                .unwrap();
        assert_eq!(after_first.received_bytes, i64::try_from(chunk).unwrap());

        // The connection drops after the second chunk's acknowledgement. The
        // client retries that chunk; the server refuses it at the offset it
        // stands at — never applies it twice.
        let after_second = append_upload_chunk(
            &db,
            &storage,
            project_id,
            session.id,
            &bytes[chunk..chunk * 2],
        )
        .await
        .unwrap();
        assert_eq!(
            after_second.received_bytes,
            i64::try_from(chunk * 2).unwrap()
        );
        let resent = append_upload_chunk(
            &db,
            &storage,
            project_id,
            session.id,
            &bytes[chunk..chunk * 2],
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                resent,
                UploadSessionError::ChunkLength { offset, expected, .. }
                    if offset == i64::try_from(chunk * 2).unwrap()
                        && expected == i64::try_from(short).unwrap()
            ),
            "the retried chunk was mis-applied: {resent:?}"
        );

        // The client re-syncs from the status and finishes with the short
        // last chunk.
        let resumed = find_upload_session(&db, project_id, session.id)
            .await
            .unwrap()
            .expect("session still in flight");
        assert_eq!(resumed.received_bytes, i64::try_from(chunk * 2).unwrap());
        append_upload_chunk(&db, &storage, project_id, session.id, &bytes[chunk * 2..])
            .await
            .unwrap();

        // Completing files the document: the stored bytes hash to the
        // declared digest — the original bytes, not a re-encoding.
        let filed = complete_upload(&db, &storage, project_id, session.id)
            .await
            .unwrap();
        let asset_id = match filed {
            crate::assets::Filed::Revision(revision) => revision.asset_id,
            crate::assets::Filed::Unchanged { .. } => {
                panic!("a first filing is a revision")
            }
        };
        let asset = crate::assets::find_by_id(&db, asset_id)
            .await
            .unwrap()
            .expect("asset row");
        assert_eq!(asset.byte_size, i64::try_from(size).unwrap());
        assert_eq!(asset.sha256_hex, sha);
        let stored = storage.get(&asset.storage_key).await.unwrap();
        assert_eq!(
            stored.bytes, bytes,
            "the filed bytes are the original bytes"
        );

        // The session and its staged parts are gone; a repeat completion
        // reports the missing session rather than re-filing.
        assert!(
            find_upload_session(&db, project_id, session.id)
                .await
                .unwrap()
                .is_none(),
            "the session row survived completion"
        );
        assert!(
            storage.get(&upload_part_key(session.id, 0)).await.is_err(),
            "a staged part survived completion"
        );
    }

    #[tokio::test]
    async fn creating_twice_for_one_upload_resumes_the_same_session() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let sha = sha256_hex(b"hearing");
        let first = create_upload_session(&db, &stream_session_args(project_id, 64, &sha))
            .await
            .unwrap();
        append_upload_chunk(&db, &storage, project_id, first.id, &[0; 64])
            .await
            .unwrap();

        let second = create_upload_session(&db, &stream_session_args(project_id, 64, &sha))
            .await
            .unwrap();
        assert_eq!(
            second.id, first.id,
            "the second create opened a second session"
        );
        assert_eq!(
            second.received_bytes, 64,
            "the resumed session lost its progress"
        );

        // A different upload identity on the same matter gets its own row.
        let other = stream_session_args(project_id, 128, &sha);
        let other = create_upload_session(&db, &other).await.unwrap();
        assert_ne!(other.id, first.id);
    }

    #[tokio::test]
    async fn an_expired_session_is_never_resumed() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let sha = sha256_hex(b"hearing");
        let session = create_upload_session(&db, &stream_session_args(project_id, 64, &sha))
            .await
            .unwrap();
        // Age the row past its TTL the way a night would.
        let past = surrealdb::types::Datetime::from(Utc::now() - chrono::Duration::hours(25));
        db.query("UPDATE $id SET expires_at = $past")
            .bind(("id", record_id(UPLOAD_SESSION_TABLE, session.id)))
            .bind(("past", past))
            .await
            .unwrap();
        assert!(
            append_upload_chunk(&db, &storage, project_id, session.id, &[0; 64])
                .await
                .is_err(),
            "an expired session accepted a chunk"
        );
        let fresh = create_upload_session(&db, &stream_session_args(project_id, 64, &sha))
            .await
            .unwrap();
        assert_ne!(
            fresh.id, session.id,
            "the expired session was resumed instead of replaced"
        );
    }

    #[tokio::test]
    async fn a_sha_mismatch_at_completion_removes_the_session_so_a_retry_starts_clean() {
        let (db, storage, _tmp, project_id) = fixtures().await;
        let wrong = sha256_hex(b"wrong bytes");
        let session = create_upload_session(&db, &stream_session_args(project_id, 11, &wrong))
            .await
            .unwrap();
        append_upload_chunk(&db, &storage, project_id, session.id, b"right bytes")
            .await
            .unwrap();

        let error = complete_upload(&db, &storage, project_id, session.id)
            .await
            .unwrap_err();
        assert!(
            matches!(error, UploadSessionError::ShaMismatch { .. }),
            "the mismatch was not reported: {error:?}"
        );
        assert!(
            find_upload_session(&db, project_id, session.id)
                .await
                .unwrap()
                .is_none(),
            "the mismatched session survived completion"
        );
        assert!(
            storage.get(&upload_part_key(session.id, 0)).await.is_err(),
            "the mismatched part survived completion"
        );
    }
}
