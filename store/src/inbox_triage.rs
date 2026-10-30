//! The triage row for one inbound Message: whether it is privileged, which
//! Project is suggested for it, and the audit trail of how both were
//! decided.
//!
//! The privilege label is structural, not statistical: a social-origin
//! channel is never privileged, and client email may only be flagged
//! privileged or left uncertain by automation. The row is a suggestion
//! until a person confirms or overrides it; every correction is recorded on
//! the row itself.

use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::inbox::InboxError;
use crate::postgres_mirror::{after_write, MirrorKey};
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "inbox_triage";
const MESSAGE: &str = "message";

const SELECT: &str = "id, message_id, firm_id, suggested_project_id, project_confidence, \
    privilege, privilege_confidence, rationale, model_id, prompt_version, raw_output, \
    disposition, labeled_example, inserted_at, updated_at";

/// Whether one inbound Message is privileged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrivilegeLabel {
    /// Client email a confident classifier flagged; the flag a person sees.
    Privileged,
    /// Social-origin channels, decided from the channel alone.
    NotPrivileged,
    /// Everything automation cannot settle — including client email below
    /// the confidence floor. These rows wait for a person.
    Uncertain,
}

impl PrivilegeLabel {
    /// The string the row stores and the review queue renders.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Privileged => "privileged",
            Self::NotPrivileged => "not_privileged",
            Self::Uncertain => "uncertain",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "privileged" => Some(Self::Privileged),
            "not_privileged" => Some(Self::NotPrivileged),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }
}

/// How far a triage row has travelled from suggestion to a human decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriageDisposition {
    /// The rule layer and classifier settled the content; a Project is
    /// suggested and waits for a person to confirm it.
    Suggested,
    /// A person confirmed the suggested Project.
    Confirmed,
    /// A person corrected the suggestion; the row is a labeled example for
    /// the next classifier generation.
    Overridden,
    /// Uncertain privilege or no confident Project — the row is in the
    /// human review queue.
    NeedsHuman,
}

impl TriageDisposition {
    /// The string the row stores and the review queue renders.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Suggested => "suggested",
            Self::Confirmed => "confirmed",
            Self::Overridden => "overridden",
            Self::NeedsHuman => "needs_human",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "suggested" => Some(Self::Suggested),
            "confirmed" => Some(Self::Confirmed),
            "overridden" => Some(Self::Overridden),
            "needs_human" => Some(Self::NeedsHuman),
            _ => None,
        }
    }
}

/// One Message's triage row as it is stored.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InboxTriage {
    pub id: Uuid,
    pub message_id: Uuid,
    pub firm_id: Uuid,
    /// A suggestion only: never applied without a person confirming it.
    pub suggested_project_id: Option<Uuid>,
    /// The classifier's confidence in that suggestion, when one was made.
    pub project_confidence: Option<f64>,
    pub privilege: PrivilegeLabel,
    /// The classifier confidence that justified [`PrivilegeLabel::Privileged`].
    /// An [`PrivilegeLabel::Uncertain`] row keeps its reasoning in
    /// `rationale` and `raw_output` instead.
    pub privilege_confidence: Option<f64>,
    /// The model's one-sentence audit artifact, when a model ran.
    pub rationale: Option<String>,
    /// The pinned classifier that produced the output, e.g.
    /// `gemini-flash-lite`; `None` when the row was decided by rules alone.
    pub model_id: Option<String>,
    /// The versioned prompt template, or [`RULES_ONLY_PROMPT`] when no
    /// model ran.
    pub prompt_version: String,
    /// The raw classifier response as it arrived, including the text of a
    /// response that failed to parse — the audit artifact for a failure.
    pub raw_output: Option<String>,
    pub disposition: TriageDisposition,
    /// Set when a person's correction turned this row into training data.
    pub labeled_example: bool,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The triage content before the row exists — what [`draft`] assembles and
/// [`record`] writes. The id and timestamps are minted at write time.
#[derive(Debug, Clone, PartialEq)]
pub struct NewInboxTriage {
    pub message_id: Uuid,
    pub firm_id: Uuid,
    pub suggested_project_id: Option<Uuid>,
    pub project_confidence: Option<f64>,
    pub privilege: PrivilegeLabel,
    pub privilege_confidence: Option<f64>,
    pub rationale: Option<String>,
    pub model_id: Option<String>,
    pub prompt_version: String,
    pub raw_output: Option<String>,
    pub disposition: TriageDisposition,
    pub labeled_example: bool,
}

#[derive(SurrealValue)]
struct TriageRow {
    id: surrealdb::types::RecordId,
    message_id: surrealdb::types::RecordId,
    firm_id: surrealdb::types::RecordId,
    suggested_project_id: Option<surrealdb::types::RecordId>,
    project_confidence: Option<f64>,
    privilege: String,
    privilege_confidence: Option<f64>,
    rationale: Option<String>,
    model_id: Option<String>,
    prompt_version: String,
    raw_output: Option<String>,
    disposition: String,
    labeled_example: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl TriageRow {
    fn into_triage(self) -> Option<InboxTriage> {
        Some(InboxTriage {
            id: record_uuid(&self.id)?,
            message_id: record_uuid(&self.message_id)?,
            firm_id: record_uuid(&self.firm_id)?,
            suggested_project_id: self.suggested_project_id.as_ref().and_then(record_uuid),
            project_confidence: self.project_confidence,
            privilege: PrivilegeLabel::parse(&self.privilege)?,
            privilege_confidence: self.privilege_confidence,
            rationale: self.rationale,
            model_id: self.model_id,
            prompt_version: self.prompt_version,
            raw_output: self.raw_output,
            disposition: TriageDisposition::parse(&self.disposition)?,
            labeled_example: self.labeled_example,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// Write the triage row for one Message. One Message has one triage row: a
/// second write for the same Message is refused, so a replayed workflow step
/// cannot overwrite the first decision. A suggested Project must belong to
/// the triaged Message's Firm — a classifier suggestion is untrusted input,
/// and cross-Firm routing is a tenancy violation, not a suggestion.
///
/// # Errors
///
/// Returns a database error, `NotFound` when the suggested Project does not
/// exist, or `CrossFirm` when it belongs to another Firm.
pub async fn record(db: &SurrealDb, triage: &NewInboxTriage) -> Result<InboxTriage, InboxError> {
    if let Some(project_id) = triage.suggested_project_id {
        let project = crate::projects::find_by_id(db, project_id)
            .await?
            .ok_or(InboxError::NotFound)?;
        if project.firm_id != Some(triage.firm_id) {
            return Err(InboxError::CrossFirm);
        }
    }
    let id = Uuid::now_v7();
    let mut response = db
        .query(format!(
            "CREATE $id SET message_id = $message, firm_id = $firm, \
             suggested_project_id = $suggested, project_confidence = $project_confidence, \
             privilege = $privilege, privilege_confidence = $privilege_confidence, \
             rationale = $rationale, model_id = $model, prompt_version = $prompt_version, \
             raw_output = $raw, disposition = $disposition, \
             labeled_example = $labeled_example, \
             inserted_at = time::now(), updated_at = time::now() RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("message", record_id(MESSAGE, triage.message_id)))
        .bind(("firm", record_id(crate::firms::TABLE, triage.firm_id)))
        .bind((
            "suggested",
            triage
                .suggested_project_id
                .map(|id| record_id(crate::projects::PROJECT_TABLE, id)),
        ))
        .bind(("project_confidence", triage.project_confidence))
        .bind(("privilege", triage.privilege.as_str().to_string()))
        .bind(("privilege_confidence", triage.privilege_confidence))
        .bind(("rationale", triage.rationale.clone()))
        .bind(("model", triage.model_id.clone()))
        .bind(("prompt_version", triage.prompt_version.clone()))
        .bind(("raw", triage.raw_output.clone()))
        .bind(("disposition", triage.disposition.as_str().to_string()))
        .bind(("labeled_example", triage.labeled_example))
        .await?
        .check()?;
    let rows: Vec<TriageRow> = response.take(0)?;
    let row = rows
        .into_iter()
        .find_map(TriageRow::into_triage)
        .ok_or(InboxError::NotFound)?;
    after_write(db, MirrorKey::InboxTriage(row.id)).await;
    Ok(row)
}

/// The triage row for one Message, when one exists. A caller that finds a
/// row here skips its own write — a deduped replay does not re-triage.
///
/// # Errors
///
/// Returns a database error.
pub async fn find_for_message(
    db: &SurrealDb,
    message_id: Uuid,
) -> Result<Option<InboxTriage>, InboxError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE message_id = $message LIMIT 1"
        ))
        .bind(("message", record_id(MESSAGE, message_id)))
        .await?
        .check()?;
    let rows: Vec<TriageRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(TriageRow::into_triage))
}

