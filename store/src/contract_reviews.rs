//! Helpers for the `contract_review` table — the per-notation work-product
//! satellite for an inbound contract review.
//!
//! The `findings` column is JSONB; this module owns the typed view
//! ([`Finding`]) and the (de)serialization. The lifecycle: [`create`] opens
//! a review at [`STATUS_PENDING`]; [`record_analysis`] writes the deviation
//! findings and risk summary ([`STATUS_ANALYZED`]); the reviewing attorney
//! edits via [`update_findings`] and closes with [`set_status`]
//! ([`STATUS_APPROVED`] / [`STATUS_REJECTED`]). Per-finding attribution (who
//! accepted what) is the matter's audit trail and lives in `notation_events`,
//! not here.
//!
//! # Storage and scope
//!
//! Commands read and write PostgreSQL whenever its host is configured. Matter scoping flows through
//! `notation_id → notation.project_id`; this table carries no `project_id`
//! of its own — every caller that needs project-level authorization
//! resolves the notation first, the same shape `store::authorities`
//! documents for `citation → authority_use.project_id`.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;
use serde_json::json;

const TABLE: &str = "contract_review";

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_ANALYZED: &str = "analyzed";
pub const STATUS_APPROVED: &str = "approved";
pub const STATUS_REJECTED: &str = "rejected";

/// One deviation the analysis found between the inbound contract and the
/// client's playbook — the unit the reviewing attorney acts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Where in the contract the deviation sits (e.g. `§7.2 Liability`).
    pub clause_ref: String,
    /// How the clause deviates from the playbook position.
    pub deviation: String,
    /// Severity: see [`crate::playbooks`] `SEVERITY_*` constants.
    pub severity: String,
    /// A suggested redline; `None` when none is proposed.
    #[serde(default)]
    pub suggested_redline: Option<String>,
    /// The reviewing attorney's note; `None` until the attorney edits.
    #[serde(default)]
    pub attorney_note: Option<String>,
    /// Whether the reviewing attorney has acted on (accepted) this finding.
    /// Defaults to `false`: nothing is accepted until the attorney acts.
    #[serde(default)]
    pub accepted: bool,
}

/// One `contract_review` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContractReview {
    pub id: Uuid,
    pub notation_id: Uuid,
    pub playbook_id: Uuid,
    /// The filed inbound-contract document; `None` until uploaded.
    pub asset_id: Option<Uuid>,
    pub status: String,
    pub risk_summary: Option<String>,
    /// The JSONB findings array — see [`findings_of`] for the typed view.
    pub findings: Json,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Why a contract-review command refused.
#[derive(Debug, thiserror::Error)]
pub enum ContractReviewError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a contract review returned no usable row")]
    WriteReturnedNothing,
    /// The JSON (de)serialization of `findings` failed — a schema/data
    /// drift, never expected at runtime.
    #[error("contract review findings JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A mutator was given an `id` that does not exist.
    #[error("contract review `{0}` not found")]
    NotFound(Uuid),
    #[error("notation event: {0}")]
    NotationEvent(#[from] crate::notation_events::NotationEventError),
}

/// What to record for one new contract review.
#[derive(Debug, Clone, Copy)]
pub struct NewContractReview {
    pub notation_id: Uuid,
    pub playbook_id: Uuid,
    /// The filed inbound-contract document; `None` until uploaded.
    pub asset_id: Option<Uuid>,
}

/// Open one `contract_review` row at [`STATUS_PENDING`] with no findings,
/// returning its id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn create(
    _db: &ApplicationStore,
    new: &NewContractReview,
) -> Result<Uuid, ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    let now = chrono::Utc::now();
    let row = ContractReview {
        id: Uuid::now_v7(),
        notation_id: new.notation_id,
        playbook_id: new.playbook_id,
        asset_id: new.asset_id,
        status: STATUS_PENDING.to_owned(),
        risk_summary: None,
        findings: json!([]),
        inserted_at: now,
        updated_at: now,
    };
    let stored: ContractReview = matter::upsert(&pg, TABLE, &row).await?;
    Ok(stored.id)
}

/// Load one contract review by id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<ContractReview>, ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(&pg, TABLE, json!({"id": id})).await?)
}

/// `notation_events.machine_kind` token for the attorney's per-finding
/// decisions. A distinct kind so these attribution rows never participate in
/// the workflow / questionnaire state-projection reads.
pub const MACHINE_CONTRACT_REVIEW: &str = "contract_review";

/// The set of finding indices that carry a recorded accept / reject decision
/// for `notation_id` — the projection of the immutable `notation_events`
/// attribution trail the review screen renders and the approve gate enforces.
///
/// # Errors
///
/// Propagates any database error.
pub async fn acted_finding_indices(
    application_store: &ApplicationStore,
    notation_id: Uuid,
) -> Result<std::collections::HashSet<usize>, ContractReviewError> {
    let events: Vec<_> = crate::notation_events::for_notation(application_store, notation_id)
        .await?
        .into_iter()
        .filter(|e| e.machine_kind == MACHINE_CONTRACT_REVIEW)
        .collect();
    Ok(events
        .iter()
        .filter_map(|e| {
            e.payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok())
                .and_then(|v| v.get("index").and_then(serde_json::Value::as_u64))
                .and_then(|i| usize::try_from(i).ok())
        })
        .collect())
}

/// The most recent contract review for a notation, if any.
///
/// # Errors
///
/// Propagates any database error.
pub async fn latest_for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<ContractReview>, ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(
        &pg,
        TABLE,
        json!({"notation_id": notation_id}),
        &["id DESC"],
    )
    .await?
    .into_iter()
    .next())
}

/// Record the analysis result: store the risk summary and findings and
/// advance the row to [`STATUS_ANALYZED`].
///
/// # Errors
///
/// [`ContractReviewError::NotFound`] if the id is unknown, or a database
/// error.
pub async fn record_analysis(
    _db: &ApplicationStore,
    id: Uuid,
    risk_summary: &str,
    findings: &[Finding],
) -> Result<(), ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    let changed: Vec<ContractReview> = matter::update(&pg, TABLE, json!({"id": id}),
        json!({"risk_summary": risk_summary, "findings": findings, "status": STATUS_ANALYZED, "updated_at": chrono::Utc::now()})).await?;
    if changed.is_empty() {
        return Err(ContractReviewError::NotFound(id));
    }
    Ok(())
}

/// Replace the findings (the reviewing attorney's per-finding edits).
///
/// # Errors
///
/// [`ContractReviewError::NotFound`] if the id is unknown, or a database
/// error.
pub async fn update_findings(
    _db: &ApplicationStore,
    id: Uuid,
    findings: &[Finding],
) -> Result<(), ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    let changed: Vec<ContractReview> = matter::update(
        &pg,
        TABLE,
        json!({"id": id}),
        json!({"findings": findings, "updated_at": chrono::Utc::now()}),
    )
    .await?;
    if changed.is_empty() {
        return Err(ContractReviewError::NotFound(id));
    }
    Ok(())
}

/// Replace the risk summary (the reviewing attorney's edit), leaving the
/// findings and status untouched.
///
/// # Errors
///
/// [`ContractReviewError::NotFound`] if the id is unknown, or a database
/// error.
pub async fn update_risk_summary(
    _db: &ApplicationStore,
    id: Uuid,
    risk_summary: &str,
) -> Result<(), ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    let changed: Vec<ContractReview> = matter::update(
        &pg,
        TABLE,
        json!({"id": id}),
        json!({"risk_summary": risk_summary, "updated_at": chrono::Utc::now()}),
    )
    .await?;
    if changed.is_empty() {
        return Err(ContractReviewError::NotFound(id));
    }
    Ok(())
}

/// Set the review status ([`STATUS_APPROVED`] / [`STATUS_REJECTED`]).
///
/// # Errors
///
/// [`ContractReviewError::NotFound`] if the id is unknown, or a database
/// error.
pub async fn set_status(
    _db: &ApplicationStore,
    id: Uuid,
    status: &str,
) -> Result<(), ContractReviewError> {
    let pg = matter::authority_for(TABLE).await?;
    let changed: Vec<ContractReview> = matter::update(
        &pg,
        TABLE,
        json!({"id": id}),
        json!({"status": status, "updated_at": chrono::Utc::now()}),
    )
    .await?;
    if changed.is_empty() {
        return Err(ContractReviewError::NotFound(id));
    }
    Ok(())
}

/// The typed findings stored on a contract-review row.
///
/// # Errors
///
/// Returns a JSON error if the stored `findings` value is not a
/// `Vec<Finding>` (a schema/data drift, never expected at runtime).
pub fn findings_of(review: &ContractReview) -> Result<Vec<Finding>, serde_json::Error> {
    serde_json::from_value(review.findings.clone())
}

#[cfg(test)]
#[path = "contract_reviews/postgres_tests.rs"]
mod postgres_tests;
