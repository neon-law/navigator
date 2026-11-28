//! Project-scoped E-Filing Transaction rows (`efiling_transactions`).
//!
//! One row per transaction an e-filing service provider (EFSP) assigned to a
//! submission, keyed by `(project_id, transaction_id)`. A write merges into the
//! existing row: fields the caller leaves unset keep their stored value.
//!
//! Two invariants keep a pending filing from reading as filed:
//!
//! - `filed_on`, `accepted_on` and `conformed_document_id` are refused unless
//!   the status is `accepted`.
//! - A notice that carries no decision (`submitted`, `unknown`) never replaces
//!   a recorded decision (`accepted`, `rejected`).

use crate::postgres::matter;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
pub use model::efiling_transactions::{
    normalize_transaction_id, STATUS_ACCEPTED, STATUS_REJECTED, STATUS_SUBMITTED, STATUS_UNKNOWN,
};

pub mod fees;

const TABLE: &str = "efiling_transactions";

/// Every status a transaction can carry.
pub const STATUSES: &[&str] = &[
    STATUS_SUBMITTED,
    STATUS_ACCEPTED,
    STATUS_REJECTED,
    STATUS_UNKNOWN,
];

/// The note an `unknown` row carries when the caller names none: the EFSP
/// said something changed but not what, so a person has to look.
pub const UNKNOWN_NOTE: &str = "status unknown — log in to the EFSP to read it";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EfilingTransaction {
    pub id: Uuid,
    pub project_id: Uuid,
    pub transaction_id: String,
    pub efsp: String,
    pub court: String,
    pub case_number: String,
    /// The submitted revision (an Asset id) this transaction carried.
    pub submitted_document_id: Option<Uuid>,
    pub status: String,
    /// The date the court deems the document filed (`YYYY-MM-DD`).
    pub filed_on: Option<String>,
    /// The date the clerk accepted the submission (`YYYY-MM-DD`).
    pub accepted_on: Option<String>,
    /// The court-stamped copy (an Asset id of kind `filing`).
    pub conformed_document_id: Option<Uuid>,
    /// A judge-signed return came back only as a received copy; the entered
    /// order still has to be obtained.
    pub needs_entered_order: bool,
    pub note: String,
    pub inserted_at: String,
    pub updated_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EfilingError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Cost(#[from] crate::project_costs::ProjectCostError),
    #[error("writing an e-filing transaction returned no usable row")]
    WriteReturnedNothing,
}

impl From<sqlx::Error> for EfilingError {
    fn from(error: sqlx::Error) -> Self {
        Self::Postgres(error.into())
    }
}

/// One status report for a transaction. `None` keeps the stored value.
#[derive(Debug, Clone, Default)]
pub struct EfilingReport {
    pub project_id: Uuid,
    pub transaction_id: String,
    pub status: String,
    pub efsp: Option<String>,
    pub court: Option<String>,
    pub case_number: Option<String>,
    pub submitted_document_id: Option<Uuid>,
    pub filed_on: Option<String>,
    pub accepted_on: Option<String>,
    pub conformed_document_id: Option<Uuid>,
    pub needs_entered_order: Option<bool>,
    pub note: Option<String>,
}

/// Whether a status records the court's decision.
fn is_decision(status: &str) -> bool {
    matches!(status, STATUS_ACCEPTED | STATUS_REJECTED)
}

fn normalize_date(name: &str, value: Option<String>) -> Result<Option<String>, EfilingError> {
    value
        .map(|raw| {
            NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
                .map(|date| date.format("%Y-%m-%d").to_string())
                .map_err(|_| EfilingError::Invalid(format!("{name} must be a YYYY-MM-DD date")))
        })
        .transpose()
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string())
}

/// Transactions on one Project, most recently changed first.
pub async fn list(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<EfilingTransaction>, EfilingError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(
        &pg,
        TABLE,
        json!({"project_id": project_id}),
        &["updated_at DESC", "id"],
    )
    .await?)
}

/// The row for one transaction id on one Project.
pub async fn find(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    transaction_id: &str,
) -> Result<Option<EfilingTransaction>, EfilingError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(
        &pg,
        TABLE,
        json!({"project_id": project_id, "transaction_id": transaction_id}),
    )
    .await?)
}

/// Record one status report and return the stored row.
///
/// # Errors
/// [`EfilingError::Invalid`] for an unknown status, a malformed date, a filed
/// date, acceptance date or conformed copy on a transaction that is not
/// `accepted`, or a document that is not on this Project.
pub async fn record(
    application_store: &ApplicationStore,
    report: &EfilingReport,
) -> Result<EfilingTransaction, EfilingError> {
    Ok(record_with_fee(application_store, report, None)
        .await?
        .filing)
}

pub async fn record_with_fee(
    _application_store: &ApplicationStore,
    report: &EfilingReport,
    receipt_id: Option<Uuid>,
) -> Result<fees::FilingWithFee, EfilingError> {
    let pg = matter::authority_for(TABLE).await?;
    record_postgres_with_fee(&pg, report, receipt_id).await
}

async fn record_postgres_with_fee(
    pg: &crate::postgres::PostgresDb,
    report: &EfilingReport,
    receipt_id: Option<Uuid>,
) -> Result<fees::FilingWithFee, EfilingError> {
    let transaction =
        normalize_transaction_id(&report.transaction_id).map_err(EfilingError::Invalid)?;
    let mut tx = pg
        .pool()
        .begin()
        .await
        .map_err(matter::MatterAuthorityError::from)?;
    let project: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM project WHERE id = $1 FOR UPDATE")
            .bind(report.project_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(matter::MatterAuthorityError::from)?;
    if project.is_none() {
        return Err(EfilingError::Invalid("no such Project".to_owned()));
    }
    let existing: Option<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM efiling_transactions t WHERE project_id = $1 AND transaction_id = $2")
        .bind(report.project_id).bind(&transaction).fetch_optional(&mut *tx).await.map_err(matter::MatterAuthorityError::from)?;
    let existing: Option<EfilingTransaction> = existing
        .map(serde_json::from_value)
        .transpose()
        .map_err(matter::MatterAuthorityError::from)?;
    let row = merge(report, transaction, existing.as_ref())?;
    for (name, asset) in [
        ("submitted document", row.submitted_document_id),
        ("conformed document", row.conformed_document_id),
    ] {
        if let Some(asset) = asset {
            let owner: Option<Option<Uuid>> =
                sqlx::query_scalar("SELECT project_id FROM asset WHERE id = $1 FOR SHARE")
                    .bind(asset)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(matter::MatterAuthorityError::from)?;
            if owner != Some(Some(report.project_id)) {
                return Err(EfilingError::Invalid(format!(
                    "{name} is not on this Project"
                )));
            }
        }
    }
    let stored: EfilingTransaction = matter::upsert_on(
        &mut *tx,
        TABLE,
        serde_json::to_value(row).map_err(matter::MatterAuthorityError::from)?,
    )
    .await?;
    let fee = if let Some(receipt_id) = receipt_id {
        Some(fees::observe(&mut tx, &stored, receipt_id).await?)
    } else {
        None
    };
    tx.commit().await?;
    Ok(fees::FilingWithFee {
        filing: stored,
        fee,
    })
}

/// Fold one report into the stored row (or a new one), enforcing that only an
/// accepted transaction carries a filed date, acceptance date or conformed
/// copy, and that a notice without a decision keeps a recorded decision.
fn merge(
    report: &EfilingReport,
    transaction_id: String,
    existing: Option<&EfilingTransaction>,
) -> Result<EfilingTransaction, EfilingError> {
    let reported = report.status.trim();
    if !STATUSES.contains(&reported) {
        return Err(EfilingError::Invalid(format!(
            "status must be one of {}",
            STATUSES.join(", ")
        )));
    }
    let keep_decision =
        existing.is_some_and(|row| is_decision(&row.status) && !is_decision(reported));
    let status = if keep_decision {
        existing.map_or(reported, |row| row.status.as_str())
    } else {
        reported
    }
    .to_string();

    let filed_on = normalize_date("filed_on", report.filed_on.clone())?
        .or_else(|| existing.and_then(|row| row.filed_on.clone()));
    let accepted_on = normalize_date("accepted_on", report.accepted_on.clone())?
        .or_else(|| existing.and_then(|row| row.accepted_on.clone()));
    let conformed_document_id = report
        .conformed_document_id
        .or_else(|| existing.and_then(|row| row.conformed_document_id));
    if status != STATUS_ACCEPTED {
        for (name, present) in [
            ("filed_on", filed_on.is_some()),
            ("accepted_on", accepted_on.is_some()),
            ("conformed_document_id", conformed_document_id.is_some()),
        ] {
            if present {
                return Err(EfilingError::Invalid(format!(
                    "{name} records a filing, so it requires status accepted (this transaction is {status})"
                )));
            }
        }
    }

    let keep = |value: &Option<String>, stored: Option<&str>| {
        trimmed(value.clone())
            .filter(|value| !value.is_empty())
            .or_else(|| stored.map(str::to_string))
            .unwrap_or_default()
    };
    let note = if keep_decision {
        existing.map(|row| row.note.clone()).unwrap_or_default()
    } else {
        match trimmed(report.note.clone()).filter(|note| !note.is_empty()) {
            Some(note) => note,
            None if status == STATUS_UNKNOWN => UNKNOWN_NOTE.to_string(),
            None => String::new(),
        }
    };
    let now = chrono::Utc::now().to_rfc3339();
    Ok(EfilingTransaction {
        id: existing.map_or_else(Uuid::now_v7, |row| row.id),
        project_id: report.project_id,
        transaction_id,
        efsp: keep(&report.efsp, existing.map(|row| row.efsp.as_str())),
        court: keep(&report.court, existing.map(|row| row.court.as_str())),
        case_number: keep(
            &report.case_number,
            existing.map(|row| row.case_number.as_str()),
        ),
        submitted_document_id: report
            .submitted_document_id
            .or_else(|| existing.and_then(|row| row.submitted_document_id)),
        status,
        filed_on,
        accepted_on,
        conformed_document_id,
        needs_entered_order: report
            .needs_entered_order
            .or_else(|| existing.map(|row| row.needs_entered_order))
            .unwrap_or(false),
        note,
        inserted_at: existing.map_or_else(|| now.clone(), |row| row.inserted_at.clone()),
        updated_at: now,
    })
}

#[cfg(test)]
mod tests {}
