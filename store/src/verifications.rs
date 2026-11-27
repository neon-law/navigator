//! `store::verifications` — recording that a licensed human checked a
//! citation, and invalidating that record when the draft moves (#891).
//!
//! Every command here keeps two properties:
//!
//! 1. **A verification names the revision it verified.** `revision_sha`
//!    is required at the type level, not merely at the column, so no
//!    caller can record an unpinned attestation.
//! 2. **Unverified is the only safe seed.** Recording an axis as passing
//!    when nobody checked it overclaims — it asserts diligence that did
//!    not happen — which is worse than having no verification at all.
//!
//! # This table lives in SurrealDB
//!
//! `verifications` moved with wave five of #1093 (ENG-121), in the
//! citation-apparatus slice. Matter scoping flows through
//! `citation_id → authority_use_id → project_id`; this table carries no
//! `project_id` of its own — see `store::authorities` for the rest of the
//! chain.

use rules::citation::{Axis, AxisStatus};
use uuid::Uuid;

use crate::application_store::ApplicationStore;

use crate::postgres::signatures as authority;

/// Why a verification command refused.
#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    /// A verification was offered without naming the revision it
    /// verified. Rejected before it reaches the database, because such a
    /// row is meaningless the moment the draft moves.
    #[error("a verification must name the draft revision it verified")]
    MissingRevision,
    /// A stored status is outside [`AxisStatus`], which means a row was
    /// written around the intended taxonomy.
    #[error("`{0}` is not a recognized axis status")]
    UnknownStatus(String),
    /// PostgreSQL, under the signature authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::SignatureFamilyError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a verification returned no usable row")]
    WriteReturnedNothing,
    /// [`set_axis`] was given a `verification_id` that does not exist.
    #[error("verification `{0}` not found")]
    NotFound(Uuid),
}

/// One verification: three independent axes, each pinned to the draft
/// revision it was checked against.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Verification {
    pub id: Uuid,
    pub citation_id: Uuid,
    pub revision_sha: String,
    pub status_citation: String,
    pub status_quote: String,
    pub status_proposition: String,
    pub verifier_person_id: Uuid,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The row as PostgreSQL's `to_jsonb` serves it, under the
/// `postgres::signatures` authority.
#[derive(serde::Deserialize)]
struct PgVerificationRow {
    id: Uuid,
    citation_id: Uuid,
    revision_sha: String,
    status_citation: String,
    status_quote: String,
    status_proposition: String,
    verifier_person_id: Uuid,
    inserted_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<PgVerificationRow> for Verification {
    fn from(row: PgVerificationRow) -> Self {
        Verification {
            id: row.id,
            citation_id: row.citation_id,
            revision_sha: row.revision_sha,
            status_citation: row.status_citation,
            status_quote: row.status_quote,
            status_proposition: row.status_proposition,
            verifier_person_id: row.verifier_person_id,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// Record a verification of `citation_id` against `revision_sha`.
///
/// All three axes start [`AxisStatus::Unverified`]. Callers move each one
/// deliberately through [`set_axis`]; there is no constructor that takes
/// a passing status, so a bulk import cannot backfill an axis as verified
/// without going through the same per-axis attestation a human would.
///
/// # Errors
/// [`VerificationError::MissingRevision`] when `revision_sha` is blank,
/// or a database error.
pub async fn record(
    _db: &ApplicationStore,
    citation_id: Uuid,
    revision_sha: &str,
    verifier_person_id: Uuid,
) -> Result<Verification, VerificationError> {
    // Checked here rather than relying on the column alone: an empty
    // string satisfies a NOT NULL constraint and names no revision.
    if revision_sha.trim().is_empty() {
        return Err(VerificationError::MissingRevision);
    }

    let pg = authority::authority().await?;
    let unverified = AxisStatus::Unverified.as_str().to_string();
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "citation_id": citation_id,
        "revision_sha": revision_sha,
        "status_citation": unverified,
        "status_quote": unverified,
        "status_proposition": unverified,
        "verifier_person_id": verifier_person_id,
    });
    let query = sqlx::query(
        "INSERT INTO verification SELECT * FROM \
         jsonb_populate_record(NULL::verification, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(verification) AS row",
    )
    .bind(row);
    authority::maybe::<PgVerificationRow>(&pg, query)
        .await?
        .map(Verification::from)
        .ok_or(VerificationError::WriteReturnedNothing)
}

async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Verification>, VerificationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query("SELECT to_jsonb(verification) AS row FROM verification WHERE id = $1")
        .bind(id);
    Ok(authority::maybe::<PgVerificationRow>(&pg, query)
        .await?
        .map(Verification::from))
}

/// Set one `axis` of `verification_id` to `status`, leaving the other two
/// untouched.
///
/// The axes are independent on purpose. A citation can be real and
/// correctly formatted, its quote accurate, and still not support the
/// assertion it is cited for — collapsing them into one control is
/// exactly what hides that case.
///
/// # Errors
/// [`VerificationError::NotFound`] when `verification_id` does not exist,
/// or a database error.
pub async fn set_axis(
    db: &ApplicationStore,
    verification_id: Uuid,
    axis: Axis,
    status: AxisStatus,
) -> Result<Verification, VerificationError> {
    if find_by_id(db, verification_id).await?.is_none() {
        return Err(VerificationError::NotFound(verification_id));
    }
    let field = match axis {
        Axis::Citation => "status_citation",
        Axis::Quote => "status_quote",
        Axis::Proposition => "status_proposition",
    };
    let pg = authority::authority().await?;
    // The field name comes from the closed `Axis` set above, never
    // from caller input, so the interpolation is a vocabulary lookup.
    let query = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE verification SET {field} = $1, updated_at = now() \
         WHERE id = $2 RETURNING to_jsonb(verification) AS row"
    )))
    .bind(status.as_str().to_string())
    .bind(verification_id);
    authority::maybe::<PgVerificationRow>(&pg, query)
        .await?
        .map(Verification::from)
        .ok_or(VerificationError::WriteReturnedNothing)
}

/// Read one axis of a stored row.
///
/// # Errors
/// [`VerificationError::UnknownStatus`] when the stored value is outside
/// the closed vocabulary.
pub fn axis_status(row: &Verification, axis: Axis) -> Result<AxisStatus, VerificationError> {
    let raw = match axis {
        Axis::Citation => &row.status_citation,
        Axis::Quote => &row.status_quote,
        Axis::Proposition => &row.status_proposition,
    };
    AxisStatus::parse(raw).ok_or_else(|| VerificationError::UnknownStatus(raw.clone()))
}

/// Every verification recorded against `citation_id`, newest first.
///
/// # Errors
/// Propagates any database error.
pub async fn for_citation(
    _db: &ApplicationStore,
    citation_id: Uuid,
) -> Result<Vec<Verification>, VerificationError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(verification) AS row FROM verification \
         WHERE citation_id = $1 ORDER BY id DESC",
    )
    .bind(citation_id);
    Ok(authority::rows::<PgVerificationRow>(&pg, query)
        .await?
        .into_iter()
        .map(Verification::from)
        .collect())
}

/// A single axis carried to [`AxisStatus::Stale`] by a draft edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTransition {
    pub verification_id: Uuid,
    pub citation_id: Uuid,
    pub axis: Axis,
    /// The revision the verification was pinned to, which is no longer
    /// the current draft.
    pub from_revision_sha: String,
}

/// Carry every verification of `citation_id` that was pinned to a
/// revision other than `current_revision_sha` to
/// [`AxisStatus::Stale`], and return the transitions.
///
/// **A draft edit must not silently retain downstream verifications.**
/// The check may still be correct, but nothing records that anyone
/// confirmed it against the current text, and a verification quietly
/// going stale is exactly the condition a filing deadline turns into a
/// problem.
///
/// Only axes that make a claim about the text move: an unverified axis
/// claims nothing and an already-stale one has nowhere further to go, so
/// neither is carried. Manufacturing those transitions would pollute the
/// staleness rate the telemetry measures.
///
/// The returned transitions are what a caller emits as telemetry —
/// identifiers, an axis, and an outcome. No quote, no citation string,
/// no proposition.
///
/// # Errors
/// Propagates any database error.
pub async fn stale_after_revision(
    _db: &ApplicationStore,
    citation_id: Uuid,
    current_revision_sha: &str,
) -> Result<Vec<StaleTransition>, VerificationError> {
    let pg = authority::authority().await?;
    Ok(
        authority::sweep_stale(&pg, citation_id, current_revision_sha)
            .await?
            .into_iter()
            .map(
                |(axis, verification_id, from_revision_sha)| StaleTransition {
                    verification_id,
                    citation_id,
                    axis,
                    from_revision_sha,
                },
            )
            .collect(),
    )
}

#[cfg(test)]
mod tests {}
