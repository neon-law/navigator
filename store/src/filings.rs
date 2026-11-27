//! `filings` — one durable record per outbound compliance submission, and
//! every query against the table.
//!
//! # This table lives in SurrealDB
//!
//! `filings` moved with wave five of #1093 (ENG-121), in the satellite-ring
//! slice.
//!
//! Written by the workflow worker inside a submission step's `ctx.run`
//! (`mailroom_send`, `certified_mail`, `e_filing`, `filing__*`), so the row
//! is the replay-idempotent proof of what was filed. A row exists only
//! after the matter passed `lawyer_review` — the workflow spec guarantees no
//! submission state is reachable without a review first
//! (`workflows::lawyer_review_precedes_submission`).

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::cases as authority;

/// Errors from the filings commands.
#[derive(Debug, thiserror::Error)]
pub enum FilingError {
    /// PostgreSQL, under the litigation authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::CasesAuthorityError),
    /// `submitted_at` is not an RFC 3339 timestamp PostgreSQL can store.
    #[error("`{0}` is not an RFC 3339 timestamp")]
    SubmittedAt(String),
    /// A write reported success but returned no row.
    #[error("writing a filing returned no usable row")]
    WriteReturnedNothing,
}

/// One durable filing record.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`FilingRow`] is the seam that turns it into (and back out of) what the
/// SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Filing {
    pub id: Uuid,
    pub notation_id: Uuid,
    /// Submission step kind / state prefix (`mailroom_send`, `certified_mail`,
    /// `e_filing`, `filing`).
    pub kind: String,
    /// Recipient office or party (e.g. `Nevada Secretary of State`).
    pub office: String,
    /// Provider/office tracking reference; `None` until one is known.
    pub reference: Option<String>,
    /// Human-readable summary of what was submitted.
    pub summary: String,
    /// RFC 3339 timestamp the submission side effect fired.
    pub submitted_at: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as PostgreSQL's `to_jsonb` serves it, under the `postgres::cases`
/// authority. `submitted_at` is a `timestamptz` column there, so it arrives
/// as a timestamp and leaves as the RFC 3339 string the type carries.
#[derive(serde::Deserialize)]
struct PgFilingRow {
    id: Uuid,
    notation_id: Uuid,
    kind: String,
    office: String,
    reference: Option<String>,
    summary: String,
    submitted_at: DateTime<Utc>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgFilingRow> for Filing {
    fn from(row: PgFilingRow) -> Self {
        Filing {
            id: row.id,
            notation_id: row.notation_id,
            kind: row.kind,
            office: row.office,
            reference: row.reference,
            summary: row.summary,
            submitted_at: row.submitted_at.to_rfc3339(),
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// What to record for one submission. `submitted_at` is stamped by the
/// caller (the worker stamps it inside the journaled step so a replay
/// reuses the same timestamp).
#[derive(Debug, Clone)]
pub struct NewFiling<'a> {
    pub notation_id: Uuid,
    pub kind: &'a str,
    pub office: &'a str,
    pub summary: &'a str,
    pub reference: Option<&'a str>,
    pub submitted_at: &'a str,
}

/// Insert one `filings` row, returning its id. No unique index guards this
/// insert, so no retry wraps it — each call mints its own fresh id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn record(_db: &ApplicationStore, new: &NewFiling<'_>) -> Result<Uuid, FilingError> {
    let pg = authority::authority().await?;
    let submitted_at = DateTime::parse_from_rfc3339(new.submitted_at)
        .map_err(|_| FilingError::SubmittedAt(new.submitted_at.to_string()))?
        .with_timezone(&Utc);
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "notation_id": new.notation_id,
        "kind": new.kind, "office": new.office,
        "reference": new.reference, "summary": new.summary,
        "submitted_at": submitted_at,
    });
    let query = sqlx::query(
        "INSERT INTO filing SELECT * FROM \
         jsonb_populate_record(NULL::filing, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING filing.id",
    )
    .bind(row);
    let id: Uuid = authority::maybe_scalar(&pg, query)
        .await?
        .ok_or(FilingError::WriteReturnedNothing)?;
    Ok(id)
}

/// All filings recorded for a notation, oldest first.
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Vec<Filing>, FilingError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(filing) AS row FROM filing \
         WHERE notation_id = $1 ORDER BY id ASC",
    )
    .bind(notation_id);
    Ok(authority::rows::<PgFilingRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[cfg(test)]
mod tests {}
