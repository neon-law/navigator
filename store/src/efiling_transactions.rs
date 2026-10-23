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

use chrono::NaiveDate;
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "efiling_transactions";

pub const STATUS_SUBMITTED: &str = "submitted";
pub const STATUS_ACCEPTED: &str = "accepted";
pub const STATUS_REJECTED: &str = "rejected";
pub const STATUS_UNKNOWN: &str = "unknown";

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

const MAX_TRANSACTION_ID: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

#[derive(SurrealValue)]
struct Row {
    id: surrealdb::types::RecordId,
    project_id: surrealdb::types::RecordId,
    transaction_id: String,
    efsp: String,
    court: String,
    case_number: String,
    submitted_document_id: Option<surrealdb::types::RecordId>,
    status: String,
    filed_on: Option<String>,
    accepted_on: Option<String>,
    conformed_document_id: Option<surrealdb::types::RecordId>,
    needs_entered_order: bool,
    note: String,
    inserted_at: String,
    updated_at: String,
}

impl Row {
    fn into_transaction(self) -> Option<EfilingTransaction> {
        Some(EfilingTransaction {
            id: record_uuid(&self.id)?,
            project_id: record_uuid(&self.project_id)?,
            transaction_id: self.transaction_id,
            efsp: self.efsp,
            court: self.court,
            case_number: self.case_number,
            submitted_document_id: self.submitted_document_id.as_ref().and_then(record_uuid),
            status: self.status,
            filed_on: self.filed_on,
            accepted_on: self.accepted_on,
            conformed_document_id: self.conformed_document_id.as_ref().and_then(record_uuid),
            needs_entered_order: self.needs_entered_order,
            note: self.note,
            inserted_at: self.inserted_at,
            updated_at: self.updated_at,
        })
    }
}

const SELECT: &str = "id, project_id, transaction_id, efsp, court, case_number, \
     submitted_document_id, status, filed_on, accepted_on, conformed_document_id, \
     needs_entered_order, note, inserted_at, updated_at";

#[derive(Debug, thiserror::Error)]
pub enum EfilingError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("writing an e-filing transaction returned no usable row")]
    WriteReturnedNothing,
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

/// Trim and check a transaction id: non-empty, no whitespace, bounded.
///
/// # Errors
/// [`EfilingError::Invalid`] naming the rule the id broke.
pub fn normalize_transaction_id(raw: &str) -> Result<String, EfilingError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(EfilingError::Invalid("transaction_id is required".into()));
    }
    if id.len() > MAX_TRANSACTION_ID || id.chars().any(char::is_whitespace) {
        return Err(EfilingError::Invalid(format!(
            "transaction_id must be at most {MAX_TRANSACTION_ID} characters with no spaces"
        )));
    }
    Ok(id.to_string())
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

async fn require_project_document(
    surreal: &SurrealDb,
    project_id: Uuid,
    document_id: Option<Uuid>,
    name: &str,
) -> Result<(), EfilingError> {
    let Some(document_id) = document_id else {
        return Ok(());
    };
    let asset = crate::assets::find_by_id(surreal, document_id)
        .await
        .map_err(|error| EfilingError::Invalid(error.to_string()))?;
    match asset {
        Some(asset) if asset.project_id == Some(project_id) => Ok(()),
        Some(_) => Err(EfilingError::Invalid(format!(
            "{name} is not on this Project"
        ))),
        None => Err(EfilingError::Invalid(format!("no such {name}"))),
    }
}

/// Transactions on one Project, most recently changed first.
pub async fn list(
    surreal: &SurrealDb,
    project_id: Uuid,
) -> Result<Vec<EfilingTransaction>, EfilingError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE project_id = $project_id \
             ORDER BY updated_at DESC, id"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<Row> = response.take(0)?;
    Ok(rows.into_iter().filter_map(Row::into_transaction).collect())
}

/// The row for one transaction id on one Project.
pub async fn find(
    surreal: &SurrealDb,
    project_id: Uuid,
    transaction_id: &str,
) -> Result<Option<EfilingTransaction>, EfilingError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM ONLY {TABLE} \
             WHERE project_id = $project_id AND transaction_id = $transaction_id LIMIT 1"
        ))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("transaction_id", transaction_id.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<Row> = response.take(0)?;
    Ok(row.and_then(Row::into_transaction))
}

/// Record one status report and return the stored row.
///
/// # Errors
/// [`EfilingError::Invalid`] for an unknown status, a malformed date, a filed
/// date, acceptance date or conformed copy on a transaction that is not
/// `accepted`, or a document that is not on this Project.
pub async fn record(
    surreal: &SurrealDb,
    report: &EfilingReport,
) -> Result<EfilingTransaction, EfilingError> {
    let transaction_id = normalize_transaction_id(&report.transaction_id)?;
    if crate::projects::find_by_id(surreal, report.project_id)
        .await
        .map_err(|error| EfilingError::Invalid(error.to_string()))?
        .is_none()
    {
        return Err(EfilingError::Invalid("no such Project".into()));
    }
    let existing = find(surreal, report.project_id, &transaction_id).await?;
    let merged = merge(report, transaction_id, existing.as_ref())?;
    require_project_document(
        surreal,
        report.project_id,
        merged.submitted_document_id,
        "submitted document",
    )
    .await?;
    require_project_document(
        surreal,
        report.project_id,
        merged.conformed_document_id,
        "conformed document",
    )
    .await?;
    persist(surreal, merged).await
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

async fn persist(
    surreal: &SurrealDb,
    row: EfilingTransaction,
) -> Result<EfilingTransaction, EfilingError> {
    let mut response = surreal
        .query(format!(
            "UPSERT $id SET project_id = $project_id, transaction_id = $transaction_id, \
             efsp = $efsp, court = $court, case_number = $case_number, \
             submitted_document_id = $submitted_document_id, status = $status, \
             filed_on = $filed_on, accepted_on = $accepted_on, \
             conformed_document_id = $conformed_document_id, \
             needs_entered_order = $needs_entered_order, note = $note, \
             inserted_at = $inserted_at, updated_at = $now RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, row.id)))
        .bind(("project_id", record_id("project", row.project_id)))
        .bind(("transaction_id", row.transaction_id))
        .bind(("efsp", row.efsp))
        .bind(("court", row.court))
        .bind(("case_number", row.case_number))
        .bind((
            "submitted_document_id",
            row.submitted_document_id.map(|id| record_id("asset", id)),
        ))
        .bind(("status", row.status))
        .bind(("filed_on", row.filed_on))
        .bind(("accepted_on", row.accepted_on))
        .bind((
            "conformed_document_id",
            row.conformed_document_id.map(|id| record_id("asset", id)),
        ))
        .bind(("needs_entered_order", row.needs_entered_order))
        .bind(("note", row.note))
        .bind(("inserted_at", row.inserted_at))
        .bind(("now", row.updated_at))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let stored: Option<Row> = response.take(0)?;
    stored
        .and_then(Row::into_transaction)
        .ok_or(EfilingError::WriteReturnedNothing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{create as create_project, NewProject};
    use crate::test_support::mem_surreal;

    async fn project(surreal: &SurrealDb) -> Uuid {
        create_project(
            surreal,
            &NewProject {
                code: format!("efiling-{}", Uuid::now_v7()),
                name: "E-filing matter".into(),
                status: "open".into(),
                entity_id: Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn report(project_id: Uuid, status: &str) -> EfilingReport {
        EfilingReport {
            project_id,
            transaction_id: "TX-0000-1111".into(),
            status: status.into(),
            efsp: Some("Example EFSP".into()),
            court: Some("Superior Court of California, County of Example".into()),
            case_number: Some("26EXCV00001".into()),
            ..EfilingReport::default()
        }
    }

    #[tokio::test]
    async fn a_report_is_upserted_by_transaction_id() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let submitted = record(&surreal, &report(project_id, STATUS_SUBMITTED))
            .await
            .unwrap();
        let mut accepted = report(project_id, STATUS_ACCEPTED);
        accepted.filed_on = Some("2026-10-05".into());
        accepted.accepted_on = Some("2026-10-06".into());
        accepted.court = None;
        let accepted = record(&surreal, &accepted).await.unwrap();
        assert_eq!(accepted.id, submitted.id);
        assert_eq!(accepted.status, STATUS_ACCEPTED);
        assert_eq!(accepted.filed_on.as_deref(), Some("2026-10-05"));
        assert_eq!(accepted.accepted_on.as_deref(), Some("2026-10-06"));
        assert_eq!(
            accepted.court, submitted.court,
            "an unset field keeps its value"
        );
        assert_eq!(list(&surreal, project_id).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_pending_transaction_is_never_recorded_as_filed() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        for status in [STATUS_SUBMITTED, STATUS_UNKNOWN, STATUS_REJECTED] {
            let mut input = report(project_id, status);
            input.filed_on = Some("2026-10-05".into());
            let error = record(&surreal, &input).await.unwrap_err();
            assert!(
                matches!(&error, EfilingError::Invalid(message) if message.contains("filed_on")),
                "{status}: {error}"
            );
            let mut input = report(project_id, status);
            input.conformed_document_id = Some(Uuid::now_v7());
            assert!(matches!(
                record(&surreal, &input).await.unwrap_err(),
                EfilingError::Invalid(_)
            ));
        }
        assert!(list(&surreal, project_id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_notice_says_to_log_in_and_keeps_a_recorded_decision() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let unknown = record(&surreal, &report(project_id, STATUS_UNKNOWN))
            .await
            .unwrap();
        assert_eq!(unknown.status, STATUS_UNKNOWN);
        assert_eq!(unknown.note, UNKNOWN_NOTE);
        assert_eq!(unknown.filed_on, None);

        let mut rejected = report(project_id, STATUS_REJECTED);
        rejected.note = Some("Missing signature".into());
        record(&surreal, &rejected).await.unwrap();
        let after = record(&surreal, &report(project_id, STATUS_UNKNOWN))
            .await
            .unwrap();
        assert_eq!(after.status, STATUS_REJECTED);
        assert_eq!(after.note, "Missing signature");
    }

    #[tokio::test]
    async fn a_bad_status_date_or_id_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        assert!(record(&surreal, &report(project_id, "filed"))
            .await
            .is_err());
        let mut input = report(project_id, STATUS_ACCEPTED);
        input.filed_on = Some("10/05/2026".into());
        assert!(record(&surreal, &input).await.is_err());
        let mut input = report(project_id, STATUS_SUBMITTED);
        input.transaction_id = "TX 1".into();
        assert!(record(&surreal, &input).await.is_err());
    }

    #[tokio::test]
    async fn a_document_from_another_project_is_refused() {
        let surreal = mem_surreal().await;
        let project_id = project(&surreal).await;
        let mut input = report(project_id, STATUS_SUBMITTED);
        input.submitted_document_id = Some(Uuid::now_v7());
        let error = record(&surreal, &input).await.unwrap_err();
        assert!(error.to_string().contains("submitted document"), "{error}");
    }
}
