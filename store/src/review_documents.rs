//! `review_documents` — the attorney-reviewed drafts a client reads and
//! comments on before signing, and every query against the table.
//!
//! # This table lives in SurrealDB
//!
//! `review_documents` moved with wave five of #1093 (ENG-121), in the
//! satellite-ring slice.
//!
//! The generation workflow inserts a draft (`status = draft`); an attorney
//! advances it to `pending_review`; the client's sign-off advances it to
//! `approved`. Kept beside the other orchestration helpers so `web` and the
//! generation workflow reach them without re-importing the entity.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::review as authority;

/// Hidden from the client; the generation workflow parks a freshly
/// generated draft here until an attorney approves it.
pub const STATUS_DRAFT: &str = "draft";
/// Visible to the scoped client, who may read and comment.
pub const STATUS_PENDING_REVIEW: &str = "pending_review";
/// The client has signed off; the draft is ready for signature.
pub const STATUS_APPROVED: &str = "approved";

/// One attorney-reviewed draft.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`ReviewDocumentRow`] is the seam that turns it into (and back out of)
/// what the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReviewDocument {
    pub id: Uuid,
    /// The matter this draft belongs to. Unique with [`Self::kind`] — one
    /// row per instrument per matter, which is what [`upsert_draft`] writes
    /// through.
    pub notation_id: Uuid,
    /// Document kind within the matter: `will`, `trust`,
    /// `directive_health`, `directive_financial`, … Unique with
    /// [`Self::notation_id`].
    pub kind: String,
    /// Human-readable title shown to the client.
    pub title: String,
    /// Attorney-reviewed draft body as sanitized HTML.
    pub body_html: String,
    /// `draft`, `pending_review`, or `approved` — see the `STATUS_*`
    /// constants.
    pub status: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as PostgreSQL's `to_jsonb` serves it, under the `postgres::review`
/// authority.
#[derive(serde::Deserialize)]
struct PgReviewDocumentRow {
    id: Uuid,
    notation_id: Uuid,
    kind: String,
    title: String,
    body_html: String,
    status: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgReviewDocumentRow> for ReviewDocument {
    fn from(row: PgReviewDocumentRow) -> Self {
        ReviewDocument {
            id: row.id,
            notation_id: row.notation_id,
            kind: row.kind,
            title: row.title,
            body_html: row.body_html,
            status: row.status,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// What to record for one reviewable draft. `status` defaults to
/// [`STATUS_DRAFT`] via [`create`] — a freshly generated draft is never
/// visible to the client until an attorney advances it.
#[derive(Debug, Clone)]
pub struct NewReviewDocument<'a> {
    pub notation_id: Uuid,
    /// Document kind within the matter (`will`, `trust`, …).
    pub kind: &'a str,
    /// Human-readable title shown to the client.
    pub title: &'a str,
    /// Attorney-reviewed draft body as sanitized HTML.
    pub body_html: &'a str,
}

/// Errors reading or writing a review document.
#[derive(Debug, thiserror::Error)]
pub enum ReviewDocumentError {
    #[error("notation: {0}")]
    Notation(#[from] crate::notations::NotationError),
    /// PostgreSQL, under the review authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::ReviewAuthorityError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a review document returned no usable row")]
    WriteReturnedNothing,
}

/// How many times [`upsert_draft`] re-reads after losing the
/// `review_document_notation_kind` slot, and the first backoff window.
///
/// Not the transaction-conflict retry — that is one policy for the whole
/// crate, in [`crate::surreal::retry`], and this module's [`writing`]
/// defers to it. This bound is around a *unique index* violation, where
/// re-running is pointless and the loop exists to re-read the winner's
/// row instead. A loser that still finds nothing after these attempts
/// has hit the same lost-create window ENG-114 tracks in
/// [`crate::persons::find_or_create`], not a budget that wants raising.
const WRITE_ATTEMPTS: usize = 5;
const WRITE_BACKOFF: std::time::Duration = std::time::Duration::from_millis(2);

/// Insert one `review_documents` row at `status = draft`, returning its id.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the insert fails (including a
/// `(notation_id, kind)` conflict — use [`upsert_draft`] when a sibling
/// row for the same instrument may already exist).
pub async fn create(
    _db: &ApplicationStore,
    new: &NewReviewDocument<'_>,
) -> Result<Uuid, ReviewDocumentError> {
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "notation_id": new.notation_id,
        "kind": new.kind, "title": new.title,
        "body_html": new.body_html, "status": STATUS_DRAFT,
    });
    let query = sqlx::query(
        "INSERT INTO review_document SELECT * FROM \
         jsonb_populate_record(NULL::review_document, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING review_document.id",
    )
    .bind(row);
    let id: Uuid = authority::maybe_scalar(&pg, query)
        .await?
        .ok_or(ReviewDocumentError::WriteReturnedNothing)?;
    Ok(id)
}

/// One review document for `(notation_id, kind)`, if any.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup fails.
pub async fn find_by_notation_kind(
    _db: &ApplicationStore,
    notation_id: Uuid,
    kind: &str,
) -> Result<Option<ReviewDocument>, ReviewDocumentError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(review_document) AS row FROM review_document \
         WHERE notation_id = $1 AND kind = $2 LIMIT 1",
    )
    .bind(notation_id)
    .bind(kind);
    Ok(authority::maybe::<PgReviewDocumentRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// Insert one `review_documents` row for `(notation_id, kind)`, or write in
/// place if a row already exists — the generation pipeline's per-instrument
/// write, so re-rendering a notation (a corrected transcript, a re-answered
/// question) never inserts a sibling row.
///
/// A row still at `draft` is refreshed: nobody has seen it yet, so the
/// newest render wins. A row that has already left `draft` (an attorney
/// released it, or the client signed off) is left untouched — the
/// generation step never overwrites reviewed content, since that would
/// bypass the human-in-the-loop gate `draft` exists to enforce. Returns the
/// row's id either way.
///
/// Race-safe without a lock, the way `crate::templates::save_version` is:
/// two concurrent renders both reading no existing row race to create one,
/// and the `review_document_notation_kind` unique index refuses the loser's
/// insert. The loser re-reads and, if the winner wrote the same instrument
/// (which it always does — both draws from the same notation/kind), treats
/// the winner's fresh draft as its own outcome and refreshes it, rather
/// than erroring on a no-op race.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup or write fails.
pub async fn upsert_draft(
    db: &ApplicationStore,
    new: &NewReviewDocument<'_>,
) -> Result<Uuid, ReviewDocumentError> {
    let mut backoff = WRITE_BACKOFF;
    for remaining in (0..WRITE_ATTEMPTS).rev() {
        if let Some(existing) = find_by_notation_kind(db, new.notation_id, new.kind).await? {
            if existing.status == STATUS_DRAFT {
                update_content(db, existing.id, new.title, new.body_html).await?;
            }
            return Ok(existing.id);
        }
        match create(db, new).await {
            Ok(id) => return Ok(id),
            Err(ReviewDocumentError::Postgres(error))
                if error.violates("review_document_notation_kind") =>
            {
                if remaining == 0 {
                    // One more read: the winner's row is there now.
                    if let Some(existing) =
                        find_by_notation_kind(db, new.notation_id, new.kind).await?
                    {
                        return Ok(existing.id);
                    }
                    return Err(ReviewDocumentError::Postgres(error));
                }
                tokio::time::sleep(rand::random_range(std::time::Duration::ZERO..=backoff)).await;
                backoff *= 2;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the last attempt returns rather than falling out of the loop")
}

async fn update_content(
    _db: &ApplicationStore,
    id: Uuid,
    title: &str,
    body_html: &str,
) -> Result<(), ReviewDocumentError> {
    let pg = authority::authority().await?;
    sqlx::query(
        "UPDATE review_document SET title = $2, body_html = $3, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(title)
    .bind(body_html)
    .execute(pg.pool())
    .await
    .map_err(authority::ReviewAuthorityError::from)?;
    Ok(())
}

/// Load one review document by id.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup fails.
pub async fn by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<ReviewDocument>, ReviewDocumentError> {
    let pg = authority::authority().await?;
    let query =
        sqlx::query("SELECT to_jsonb(review_document) AS row FROM review_document WHERE id = $1")
            .bind(id);
    Ok(authority::maybe::<PgReviewDocumentRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// All review documents for a notation, oldest first.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup fails.
pub async fn for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Vec<ReviewDocument>, ReviewDocumentError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(review_document) AS row FROM review_document \
         WHERE notation_id = $1 ORDER BY id ASC",
    )
    .bind(notation_id);
    Ok(authority::rows::<PgReviewDocumentRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Which of `notation_ids` have at least one drafted instrument — the
/// batched, **content-free** existence probe the matter-lifecycle
/// indicator reads.
///
/// One round trip for the whole set, never one per notation, because the
/// caller ([`crate::projects::matter_lifecycle_sets`]) renders a list and
/// its query budget is per-table.
///
/// Projects `notation_id` alone and returns bare ids. That is deliberate,
/// not an optimization: [`ReviewDocument::body_html`] is a drafted legal
/// instrument — a will, a trust, a health-care directive — and the caller
/// needs to know only *whether* one exists. Selecting the body would pull
/// the most sensitive text the firm holds into a list-page request that
/// has no use for it.
///
/// Every status counts, `draft` included. A draft sitting in attorney
/// review is an instrument the walk actually produced; whether the
/// attorney has approved it yet is a different question from whether it
/// exists.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup fails.
pub async fn notations_with_drafts(
    _db: &ApplicationStore,
    notation_ids: &[Uuid],
) -> Result<std::collections::HashSet<Uuid>, ReviewDocumentError> {
    if notation_ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let pg = authority::authority().await?;
    let query =
        sqlx::query("SELECT DISTINCT notation_id FROM review_document WHERE notation_id = ANY($1)")
            .bind(notation_ids.to_vec());
    Ok(authority::scalars::<Uuid>(&pg, query)
        .await?
        .into_iter()
        .collect())
}

/// All client-visible review documents for a project — those whose
/// notation belongs to the project and whose status has been advanced past
/// `draft`.
///
/// Resolves the project's notation ids first, then filters by that set —
/// two round trips rather than a join, the way `notation` (a different
/// table) never joins across engines any more.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the lookup fails.
pub async fn client_visible_for_project(
    db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<ReviewDocument>, ReviewDocumentError> {
    let notation_ids: Vec<Uuid> = crate::notations::list_by_project(db, project_id)
        .await?
        .into_iter()
        .map(|n| n.id)
        .collect();
    if notation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(review_document) AS row FROM review_document \
         WHERE notation_id = ANY($1) AND status <> $2 ORDER BY id ASC",
    )
    .bind(notation_ids.clone())
    .bind(STATUS_DRAFT);
    Ok(authority::rows::<PgReviewDocumentRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Move a review document to a new `status`. Returns the updated row, or
/// `Ok(None)` if no row matched.
///
/// # Errors
///
/// [`ReviewDocumentError::Db`] if the write fails.
pub async fn set_status(
    _db: &ApplicationStore,
    id: Uuid,
    status: &str,
) -> Result<Option<ReviewDocument>, ReviewDocumentError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE review_document SET status = $2, updated_at = now() WHERE id = $1 \
         RETURNING to_jsonb(review_document) AS row",
    )
    .bind(id)
    .bind(status);
    Ok(authority::maybe::<PgReviewDocumentRow>(&pg, query)
        .await?
        .map(Into::into))
}

#[cfg(test)]
mod tests {}
