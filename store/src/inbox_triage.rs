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

    /// Read the label back from the stored or form-posted string. The human
    /// review queue's resolve form posts one of these.
    ///
    /// # Errors
    ///
    /// Returns `None` for any other string.
    #[must_use]
    pub fn from_form(value: &str) -> Option<Self> {
        Self::parse(value)
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// The prompt version recorded on rows the rule layer decided without a
/// model. The hosted classifier's prompt template is versioned separately
/// when that lane lands.
pub const RULES_ONLY_PROMPT: &str = "rules-only-v1";

/// The least classifier confidence that may flag client email
/// [`PrivilegeLabel::Privileged`]. Anything below it is
/// [`PrivilegeLabel::Uncertain`], and the row waits for a person.
pub const PRIVILEGED_CONFIDENCE_FLOOR: f64 = 0.90;

/// Whether the hosted classifier lane may be consulted for a message on this
/// channel at all. Social-origin channels are settled by the rule layer
/// alone; only client email and SMS may spend a model call. The workflow
/// branches on this before its classifier step so a replay journal never
/// contains a social message's provider invocation.
#[must_use]
pub fn classifier_may_run(channel: &str) -> bool {
    !crate::inbox::is_social_channel(channel)
}

/// The schema-validated privilege verdict from the hosted classifier, as
/// the workflow lane will hand it over. Malformed or missing output never
/// becomes a `ClassifierOutput` — it stays `None`, and the label is
/// [`PrivilegeLabel::Uncertain`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClassifierOutput {
    pub privileged: bool,
    pub privilege_confidence: f64,
}

/// Decide the privilege label for one inbound Message. Structural, not
/// statistical.
///
/// A social-origin channel is never privileged: the label comes from the
/// channel alone and `classifier` is never consulted for one. Client email
/// and SMS may be flagged privileged only when a classifier ran and its
/// confidence reaches [`PRIVILEGED_CONFIDENCE_FLOOR`]; every other
/// automated outcome — no classifier, a "not privileged" verdict, low
/// confidence — is [`PrivilegeLabel::Uncertain`]. Automation can never
/// produce [`PrivilegeLabel::NotPrivileged`] for client email: that label
/// belongs to social-origin channels and to a person's review.
#[must_use]
pub fn privilege_label(channel: &str, classifier: Option<&ClassifierOutput>) -> PrivilegeLabel {
    if crate::inbox::is_social_channel(channel) {
        return PrivilegeLabel::NotPrivileged;
    }
    match classifier {
        Some(output)
            if output.privileged && output.privilege_confidence >= PRIVILEGED_CONFIDENCE_FLOOR =>
        {
            PrivilegeLabel::Privileged
        }
        _ => PrivilegeLabel::Uncertain,
    }
}

/// The disposition automation may give a triage row:
/// [`TriageDisposition::Suggested`] only when the label is settled and a
/// Project suggestion exists, and [`TriageDisposition::NeedsHuman`]
/// otherwise — uncertain privilege always waits for a person, and so does a
/// row with nothing to confirm. [`TriageDisposition::Confirmed`] and
/// [`TriageDisposition::Overridden`] are human acts and are never produced
/// here.
#[must_use]
pub fn automated_disposition(
    privilege: PrivilegeLabel,
    suggested_project_id: Option<Uuid>,
) -> TriageDisposition {
    match (privilege, suggested_project_id) {
        (PrivilegeLabel::Uncertain, _) | (_, None) => TriageDisposition::NeedsHuman,
        (_, Some(_)) => TriageDisposition::Suggested,
    }
}

/// One triage row joined with the message context the review queue renders.
/// Triage records never journal message text, so the queue renders what the
/// row and the conversation hold: the sender, channel, and Brand the ingest
/// recorded, plus the classifier's suggestion and its audit rationale.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TriageReviewRow {
    pub triage: InboxTriage,
    pub conversation_id: Uuid,
    pub sender: String,
    pub channel: String,
    pub brand_key: Option<String>,
    /// The suggested Project's name, when the suggestion still resolves.
    pub suggested_project_name: Option<String>,
}

/// The dispositions that still wait for a person: the rule layer or a
/// classifier left the row at [`TriageDisposition::NeedsHuman`], or the row
/// carries a suggestion a person has yet to confirm.
fn awaits_a_person(disposition: TriageDisposition) -> bool {
    matches!(
        disposition,
        TriageDisposition::NeedsHuman | TriageDisposition::Suggested
    )
}

/// The triage rows awaiting a person, visible to this actor. The same
/// firm-scoped visibility the inbox list uses: an Owner reads every Firm's
/// rows, the other firm tiers read the Firms they belong to, and a Client is
/// denied. Rows whose decision is already recorded are gone from the queue.
///
/// # Errors
///
/// [`InboxError::Denied`] for a Client; database errors otherwise.
pub async fn list_for_review(
    db: &SurrealDb,
    actor_id: Uuid,
) -> Result<Vec<TriageReviewRow>, InboxError> {
    let actor = crate::persons::find_by_id(db, actor_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let visible_firms: Option<Vec<Uuid>> = if actor.role == crate::persons::Role::Owner {
        None
    } else if matches!(
        actor.role,
        crate::persons::Role::Admin | crate::persons::Role::Lawyer | crate::persons::Role::Clerk
    ) {
        Some(
            crate::firms::memberships_for_person(db, actor_id)
                .await
                .map_err(|error| InboxError::Db(error.to_string()))?
                .into_iter()
                .map(|membership| membership.firm_id)
                .collect(),
        )
    } else {
        return Err(InboxError::Denied);
    };
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE disposition = 'needs_human' OR disposition = 'suggested' \
             ORDER BY updated_at DESC"
        ))
        .await?
        .check()?;
    let rows: Vec<TriageRow> = response.take(0)?;
    let mut review_rows = Vec::new();
    for row in rows {
        let Some(triage) = row.into_triage() else {
            continue;
        };
        if let Some(firms) = &visible_firms {
            if !firms.contains(&triage.firm_id) {
                continue;
            }
        }
        let Some(row) = review_row(db, triage).await? else {
            continue;
        };
        review_rows.push(row);
    }
    Ok(review_rows)
}

/// The review context for one triage row: the conversation its message
/// landed in and the suggested Project's name.
async fn review_row(
    db: &SurrealDb,
    triage: InboxTriage,
) -> Result<Option<TriageReviewRow>, InboxError> {
    let Ok(conversation) = conversation_for_message(db, triage.message_id).await else {
        // A message whose conversation vanished leaves no queue entry to
        // act on — the row is orphaned, not a routing decision.
        return Ok(None);
    };
    let suggested_project_name = match triage.suggested_project_id {
        Some(project_id) => crate::projects::find_by_id(db, project_id)
            .await?
            .map(|project| project.name),
        None => None,
    };
    Ok(Some(TriageReviewRow {
        sender: conversation
            .external_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| conversation.external_address.clone()),
        channel: conversation.channel.clone(),
        brand_key: conversation.brand_key.clone(),
        conversation_id: conversation.id,
        suggested_project_name,
        triage,
    }))
}

/// The conversation one triaged Message landed in — the row's channel and
/// sender live there, not on the triage record.
async fn conversation_for_message(
    db: &SurrealDb,
    message_id: Uuid,
) -> Result<crate::inbox::Conversation, InboxError> {
    let message = crate::inbox::message_by_id(db, message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    crate::inbox::conversation_by_id(db, message.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)
}

/// One triage row by its id.
///
/// # Errors
///
/// Returns a database error.
pub async fn find_by_id(db: &SurrealDb, triage_id: Uuid) -> Result<Option<InboxTriage>, InboxError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM $id"))
        .bind(("id", record_id(TABLE, triage_id)))
        .await?
        .check()?;
    let rows: Vec<TriageRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(TriageRow::into_triage))
}

/// A person's decision on one triage row — what the review actions write.
struct ReviewDecision {
    privilege: PrivilegeLabel,
    privilege_confidence: Option<f64>,
    suggested_project_id: Option<Uuid>,
    project_confidence: Option<f64>,
    rationale: Option<String>,
    disposition: TriageDisposition,
    labeled_example: bool,
}

/// Write a person's decision onto the row and mirror it. The audit trail is
/// the row itself: the new disposition, the decision's rationale, and — for
/// a correction — the labeled-example flag.
async fn write_review_decision(
    db: &SurrealDb,
    triage_id: Uuid,
    decision: ReviewDecision,
) -> Result<InboxTriage, InboxError> {
    let mut response = db
        .query(format!(
            "UPDATE $id SET privilege = $privilege, \
             privilege_confidence = $privilege_confidence, \
             suggested_project_id = $suggested, \
             project_confidence = $project_confidence, rationale = $rationale, \
             disposition = $disposition, labeled_example = $labeled_example, \
             updated_at = time::now() RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, triage_id)))
        .bind(("privilege", decision.privilege.as_str().to_string()))
        .bind(("privilege_confidence", decision.privilege_confidence))
        .bind((
            "suggested",
            decision
                .suggested_project_id
                .map(|id| record_id(crate::projects::PROJECT_TABLE, id)),
        ))
        .bind(("project_confidence", decision.project_confidence))
        .bind(("rationale", decision.rationale.clone()))
        .bind(("disposition", decision.disposition.as_str().to_string()))
        .bind(("labeled_example", decision.labeled_example))
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

/// A row a person may still decide: never one whose decision is recorded.
fn require_open(row: &InboxTriage) -> Result<(), InboxError> {
    if awaits_a_person(row.disposition) {
        Ok(())
    } else {
        Err(InboxError::Invalid(
            "the triage decision is already recorded; re-run triage to reopen it".into(),
        ))
    }
}

/// A person confirms the suggested Project: the conversation is attributed
/// to it through the participation-checked assignment door, and the row's
/// disposition records the confirmation. Confirm assigns the suggested
/// Project only — a different choice is an override.
///
/// # Errors
///
/// [`InboxError::NotFound`] when the row or Project does not exist,
/// [`InboxError::Invalid`] when the row is decided or names no suggestion,
/// [`InboxError::Denied`] when the actor fails the review authority, and
/// [`InboxError::CrossFirm`] when the suggestion crosses Firms.
pub async fn confirm_project(
    db: &SurrealDb,
    actor_id: Uuid,
    triage_id: Uuid,
    project_id: Uuid,
) -> Result<InboxTriage, InboxError> {
    let row = find_by_id(db, triage_id).await?.ok_or(InboxError::NotFound)?;
    require_open(&row)?;
    // Authorize before reading any suggestion state: an unauthorized actor
    // is denied outright and learns nothing about the row.
    crate::inbox::require_triage_reviewer(db, actor_id, project_id).await?;
    let Some(suggested) = row.suggested_project_id else {
        return Err(InboxError::Invalid(
            "the triage row names no suggested project to confirm".into(),
        ));
    };
    if project_id != suggested {
        return Err(InboxError::Invalid(
            "confirm assigns the suggested project only; choose override to correct it".into(),
        ));
    }
    let assignment = crate::inbox::assign_for_triage_review(
        db,
        actor_id,
        row_conversation_id(db, &row).await?,
        project_id,
    )
    .await?;
    write_review_decision(
        db,
        triage_id,
        ReviewDecision {
            privilege: row.privilege,
            privilege_confidence: row.privilege_confidence,
            suggested_project_id: Some(assignment.to_project_id),
            project_confidence: row.project_confidence,
            rationale: row.rationale.clone(),
            disposition: TriageDisposition::Confirmed,
            labeled_example: row.labeled_example,
        },
    )
    .await
}

/// A person corrects the suggestion: the conversation is attributed to the
/// corrected Project through the same door, the correction itself is the
/// labeled example the next classifier generation trains on, and the row's
/// disposition records the override.
///
/// # Errors
///
/// [`InboxError::NotFound`] when the row or Project does not exist,
/// [`InboxError::Invalid`] when the row is decided or the rationale is
/// blank, [`InboxError::Denied`] when the actor fails the review authority,
/// and [`InboxError::CrossFirm`] when the correction crosses Firms.
pub async fn override_project(
    db: &SurrealDb,
    actor_id: Uuid,
    triage_id: Uuid,
    project_id: Uuid,
    rationale: &str,
) -> Result<InboxTriage, InboxError> {
    let rationale = rationale.trim();
    if rationale.is_empty() {
        return Err(InboxError::Invalid("an override carries a rationale".into()));
    }
    let row = find_by_id(db, triage_id).await?.ok_or(InboxError::NotFound)?;
    require_open(&row)?;
    let project = crate::projects::find_by_id(db, project_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(row.firm_id) {
        return Err(InboxError::CrossFirm);
    }
    let assignment = crate::inbox::assign_for_triage_review(
        db,
        actor_id,
        row_conversation_id(db, &row).await?,
        project_id,
    )
    .await?;
    write_review_decision(
        db,
        triage_id,
        ReviewDecision {
            privilege: row.privilege,
            privilege_confidence: row.privilege_confidence,
            suggested_project_id: Some(assignment.to_project_id),
            // A human decision carries no classifier confidence.
            project_confidence: None,
            // The model's rationale survives in `raw_output`; the row
            // carries the human decision that superseded it.
            rationale: Some(format!("human override: {rationale}")),
            disposition: TriageDisposition::Overridden,
            labeled_example: true,
        },
    )
    .await
}

/// A person settles an uncertain privilege label. The rule layer stays
/// authoritative on both sides of the form: a social-origin channel is
/// forced back to [`PrivilegeLabel::NotPrivileged`] whatever the form
/// posts, and the disposition is recomputed from the settled label. The
/// human's reason is kept on the row; the classifier's own artifact
/// survives in `raw_output`.
///
/// # Errors
///
/// [`InboxError::NotFound`] when the row does not exist,
/// [`InboxError::Invalid`] when the row is decided, the label is not a
/// resolution, or the rationale is blank, and [`InboxError::Denied`] when
/// the actor fails the review authority.
pub async fn resolve_privilege(
    db: &SurrealDb,
    actor_id: Uuid,
    triage_id: Uuid,
    requested: PrivilegeLabel,
    rationale: &str,
) -> Result<InboxTriage, InboxError> {
    let rationale = rationale.trim();
    if rationale.is_empty() {
        return Err(InboxError::Invalid("a resolution carries a rationale".into()));
    }
    let row = find_by_id(db, triage_id).await?.ok_or(InboxError::NotFound)?;
    require_open(&row)?;
    let actor = crate::persons::find_by_id(db, actor_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !crate::inbox::triage_reviewer_may_resolve(db, &actor, row.firm_id).await? {
        return Err(InboxError::Denied);
    }
    let channel = conversation_for_message(db, row.message_id)
        .await?
        .channel;
    // The structural rule outranks the form: a social-origin channel is
    // never privileged, however the human asked.
    let privilege = if crate::inbox::is_social_channel(&channel) {
        PrivilegeLabel::NotPrivileged
    } else {
        match requested {
            PrivilegeLabel::Uncertain => {
                return Err(InboxError::Invalid(
                    "a resolution settles the label; uncertain is what the queue is for".into(),
                ));
            }
            settled => settled,
        }
    };
    write_review_decision(
        db,
        triage_id,
        ReviewDecision {
            privilege,
            privilege_confidence: None,
            suggested_project_id: row.suggested_project_id,
            project_confidence: row.project_confidence,
            rationale: Some(format!("human review: {rationale}")),
            disposition: automated_disposition(privilege, row.suggested_project_id),
            labeled_example: row.labeled_example,
        },
    )
    .await
}

/// A fresh pass of the rule layer over a triage row: the label is
/// recomputed from the message's channel alone — a social-origin channel
/// lands `NotPrivileged`, client email and SMS land `Uncertain` (the
/// hosted classifier's re-run rides the workflow lane, not the portal) —
/// and the disposition follows. A row a person resolved returns to the
/// queue; an existing labeled example stays labeled.
///
/// # Errors
///
/// [`InboxError::NotFound`] when the row does not exist and
/// [`InboxError::Denied`] when the actor fails the review authority.
pub async fn rerun(
    db: &SurrealDb,
    actor_id: Uuid,
    triage_id: Uuid,
) -> Result<InboxTriage, InboxError> {
    let row = find_by_id(db, triage_id).await?.ok_or(InboxError::NotFound)?;
    let actor = crate::persons::find_by_id(db, actor_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !crate::inbox::triage_reviewer_may_resolve(db, &actor, row.firm_id).await? {
        return Err(InboxError::Denied);
    }
    let channel = conversation_for_message(db, row.message_id)
        .await?
        .channel;
    let privilege = privilege_label(&channel, None);
    write_review_decision(
        db,
        triage_id,
        ReviewDecision {
            privilege,
            privilege_confidence: None,
            suggested_project_id: row.suggested_project_id,
            project_confidence: row.project_confidence,
            rationale: row.rationale.clone(),
            disposition: automated_disposition(privilege, row.suggested_project_id),
            labeled_example: row.labeled_example,
        },
    )
    .await
}

/// The conversation id behind a triage row — the assignment doors need it.
async fn row_conversation_id(
    db: &SurrealDb,
    row: &InboxTriage,
) -> Result<Uuid, InboxError> {
    Ok(conversation_for_message(db, row.message_id).await?.id)
}

#[cfg(test)]
mod tests {
    use super::{
        automated_disposition, find_for_message, privilege_label, record, ClassifierOutput,
        NewInboxTriage, PrivilegeLabel, TriageDisposition, RULES_ONLY_PROMPT,
    };
    use crate::inbox::InboxError;
    use uuid::Uuid;

    fn classifier(privileged: bool, confidence: f64) -> ClassifierOutput {
        ClassifierOutput {
            privileged,
            privilege_confidence: confidence,
        }
    }

    /// The channel the classifier is never consulted for: every verdict,
    /// including a confident "this is privileged", is refused.
    #[test]
    fn social_channels_ignore_the_classifier() {
        for channel in ["facebook", "instagram", "threads", "youtube", "tiktok"] {
            for verdict in [
                Some(classifier(true, 1.0)),
                Some(classifier(false, 1.0)),
                Some(classifier(true, 0.0)),
                None,
            ] {
                assert_eq!(
                    privilege_label(channel, verdict.as_ref()),
                    PrivilegeLabel::NotPrivileged,
                    "{channel} must settle privilege from the channel alone"
                );
            }
        }
    }

    #[test]
    fn client_email_at_or_above_the_floor_is_flagged() {
        assert_eq!(
            privilege_label("email", Some(&classifier(true, 0.90))),
            PrivilegeLabel::Privileged
        );
        assert_eq!(
            privilege_label("sms", Some(&classifier(true, 0.99))),
            PrivilegeLabel::Privileged
        );
    }

    #[test]
    fn client_email_below_the_floor_is_uncertain() {
        assert_eq!(
            privilege_label("email", Some(&classifier(true, 0.89))),
            PrivilegeLabel::Uncertain
        );
    }

    /// The invariant that matters: automation's only outcomes for client
    /// email are Privileged and Uncertain — never NotPrivileged, however
    /// confident a "not privileged" verdict claims to be.
    #[test]
    fn automation_never_calls_client_email_not_privileged() {
        for channel in ["email", "sms"] {
            for verdict in [
                Some(classifier(false, 1.0)),
                Some(classifier(false, 0.99)),
                Some(classifier(true, 0.5)),
                None,
            ] {
                assert_eq!(
                    privilege_label(channel, verdict.as_ref()),
                    PrivilegeLabel::Uncertain,
                    "{channel} must stay uncertain without a confident classifier"
                );
            }
        }
    }

    #[test]
    fn uncertain_privilege_and_missing_suggestions_need_a_human() {
        assert_eq!(
            automated_disposition(PrivilegeLabel::Uncertain, Some(Uuid::now_v7())),
            TriageDisposition::NeedsHuman
        );
        assert_eq!(
            automated_disposition(PrivilegeLabel::Privileged, None),
            TriageDisposition::NeedsHuman
        );
        assert_eq!(
            automated_disposition(PrivilegeLabel::NotPrivileged, None),
            TriageDisposition::NeedsHuman
        );
        assert_eq!(
            automated_disposition(PrivilegeLabel::NotPrivileged, Some(Uuid::now_v7())),
            TriageDisposition::Suggested
        );
    }

    async fn inbound(
        db: &crate::surreal::SurrealDb,
        firm_id: Uuid,
        channel: &'static str,
        provider_message_id: &'static str,
    ) -> crate::inbox::IngestedInbound {
        crate::inbox::ingest_inbound(
            db,
            &crate::inbox::IngestInbound {
                firm_id,
                brand_key: Some("neon"),
                interaction_type: None,
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: None,
                channel,
                provider_account: "synthetic-account",
                provider_message_id,
                provider_thread_ref: None,
                external_address: "sender@example.com",
                external_name: None,
                subject: None,
                body: "Synthetic message",
                source_page: None,
                source_channel: None,
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachment_asset_id: None,
                occurred_at: "2026-10-20T00:00:00Z",
            },
        )
        .await
        .unwrap()
    }

    async fn seed_project(
        db: &crate::surreal::SurrealDb,
        firm_id: Uuid,
        name: &str,
    ) -> crate::projects::Project {
        crate::projects::create(
            db,
            &crate::projects::NewProject {
                code: crate::projects::code_from_name(name, Uuid::now_v7()),
                name: name.into(),
                status: "open".into(),
                entity_id: crate::test_support::seed_entity(db).await,
                firm_id: Some(firm_id),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    #[allow(clippy::too_many_lines)]
    fn new_triage(
        message_id: Uuid,
        firm_id: Uuid,
        suggested_project_id: Option<Uuid>,
        privilege: PrivilegeLabel,
        disposition: TriageDisposition,
    ) -> NewInboxTriage {
        // Rules-layer rows: no model ran, so the model fields stay unset —
        // the classifier lane fills them in a later deliverable.
        NewInboxTriage {
            message_id,
            firm_id,
            suggested_project_id,
            project_confidence: suggested_project_id.map(|_| 0.8),
            privilege,
            privilege_confidence: None,
            rationale: Some("synthetic rationale".into()),
            model_id: None,
            prompt_version: RULES_ONLY_PROMPT.into(),
            raw_output: None,
            disposition,
            labeled_example: false,
        }
    }

    /// No classifier output: the email is Uncertain and the row waits for a
    /// person. Ingest itself is never blocked — the triage row records the
    /// wait.
    #[tokio::test]
    async fn unclassified_email_lands_needs_human() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let message = inbound(&db, firm, "email", "triage-uncertain").await;
        let label = privilege_label("email", None);
        assert_eq!(label, PrivilegeLabel::Uncertain);
        let row = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                None,
                label,
                automated_disposition(label, None),
            ),
        )
        .await
        .unwrap();
        assert_eq!(row.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(row.disposition, TriageDisposition::NeedsHuman);
        assert_eq!(row.model_id, None);
        assert_eq!(row.prompt_version, RULES_ONLY_PROMPT);
        let found = find_for_message(&db, message.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found, row);
    }

    /// A social-origin message settles at the rule layer: NotPrivileged with
    /// a suggested Project, and no model fields.
    #[tokio::test]
    async fn social_triage_settles_without_a_model() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let message = inbound(&db, firm, "instagram", "triage-social").await;
        let label = privilege_label("instagram", None);
        assert_eq!(label, PrivilegeLabel::NotPrivileged);
        let project = seed_project(&db, firm, "Synthetic Triage Project").await;
        let row = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                Some(project.id),
                label,
                automated_disposition(label, Some(project.id)),
            ),
        )
        .await
        .unwrap();
        assert_eq!(row.privilege, PrivilegeLabel::NotPrivileged);
        assert_eq!(row.disposition, TriageDisposition::Suggested);
        assert_eq!(row.privilege_confidence, None);
    }

    /// A suggested Project from another Firm is a tenancy violation, not a
    /// suggestion — refused before the row exists.
    #[tokio::test]
    async fn cross_firm_project_suggestions_are_refused() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        // The suggestion check compares Project ownership, so the second
        // Firm needs no Brand — just itself and a Project.
        let other_admin = crate::persons::create(
            &db,
            &crate::persons::NewPerson::with_role(
                "Test Other Firm Admin",
                format!("test-other-firm-admin-{}@example.com", Uuid::now_v7()),
                crate::persons::Role::Admin,
            ),
        )
        .await
        .unwrap();
        let other = crate::firms::create(
            &db,
            &crate::firms::NewFirm {
                name: format!("Test Other Firm {}", Uuid::now_v7()),
                status: "active".into(),
                entity_id: crate::test_support::seed_entity(&db).await,
                admin_dri_person_id: other_admin.id,
            },
        )
        .await
        .unwrap();
        let message = inbound(&db, firm, "email", "triage-cross-firm").await;
        let project = seed_project(&db, other.id, "Synthetic Other Firm Project").await;
        let error = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                Some(project.id),
                PrivilegeLabel::Uncertain,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, InboxError::CrossFirm));
        assert!(find_for_message(&db, message.message_id)
            .await
            .unwrap()
            .is_none());
    }

    /// One Message has one triage row: a replayed write is refused and the
    /// first decision stands.
    #[tokio::test]
    async fn a_replayed_triage_does_not_overwrite_the_first() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let message = inbound(&db, firm, "email", "triage-replay").await;
        let first = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                None,
                PrivilegeLabel::Uncertain,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await
        .unwrap();
        let replay = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                None,
                PrivilegeLabel::Privileged,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await;
        assert!(replay.is_err());
        let found = find_for_message(&db, message.message_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found, first);
    }

    // -- The human review queue -------------------------------------------------

    use super::{
        confirm_project, conversation_for_message, find_by_id, list_for_review, override_project,
        rerun, resolve_privilege, write_review_decision, ReviewDecision,
    };
    use crate::firms::{FirmMembership, NewPersonFirmRole};
    use crate::persons::{self, NewPerson, Role};

    async fn person(db: &crate::surreal::SurrealDb, name: &str, role: Role) -> Uuid {
        persons::create(
            db,
            &NewPerson::with_role(
                name,
                format!(
                    "{}-{}@example.com",
                    name.to_lowercase().replace(" ", "-"),
                    Uuid::now_v7()
                ),
                role,
            ),
        )
        .await
        .unwrap()
        .id
    }

    async fn member(
        db: &crate::surreal::SurrealDb,
        person_id: Uuid,
        firm_id: Uuid,
        membership: FirmMembership,
    ) {
        crate::firms::ensure_membership(
            db,
            &NewPersonFirmRole {
                person_id,
                firm_id,
                membership,
                is_dri: false,
            },
        )
        .await
        .unwrap();
    }

    /// Give the lawyer a firm-side participation row on the Project — the
    /// row `can_see_project` resolves for the participation check.
    async fn participate(db: &crate::surreal::SurrealDb, person_id: Uuid, project_id: Uuid) {
        crate::projects::add_participation(
            db,
            project_id,
            person_id,
            crate::projects::participation_for_role(Role::Lawyer),
        )
        .await
        .unwrap();
    }

    /// A NeedsHuman email row whose classifier suggested `project`.
    async fn needs_human_with_suggestion(
        db: &crate::surreal::SurrealDb,
        firm: Uuid,
        tag: &'static str,
        project: Option<Uuid>,
    ) -> (Uuid, crate::inbox::IngestedInbound) {
        let message = inbound(db, firm, "email", tag).await;
        let row = record(
            db,
            &new_triage(
                message.message_id,
                firm,
                project,
                PrivilegeLabel::Uncertain,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await
        .unwrap();
        (row.id, message)
    }

    #[tokio::test]
    async fn the_queue_lists_awaiting_rows_and_hides_the_rest() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let other = crate::test_support::seed_firm_with_brand(&db, "lawyer-shook").await;
        let owner = person(&db, "Queue Owner", Role::Owner).await;
        let lawyer = person(&db, "Queue Lawyer", Role::Lawyer).await;
        member(&db, lawyer, firm, FirmMembership::Lawyer).await;
        let (_, first) = needs_human_with_suggestion(&db, firm, "queue-first", None).await;
        needs_human_with_suggestion(&db, other, "queue-other", None).await;
        // A row a person already decided never waits in the queue.
        let (decided, _) = needs_human_with_suggestion(&db, firm, "queue-decided", None).await;
        let decided_row = find_by_id(&db, decided).await.unwrap().unwrap();
        write_review_decision(
            &db,
            decided,
            ReviewDecision {
                privilege: decided_row.privilege,
                privilege_confidence: decided_row.privilege_confidence,
                suggested_project_id: None,
                project_confidence: None,
                rationale: Some("human review: no project applies".into()),
                disposition: TriageDisposition::Overridden,
                labeled_example: true,
            },
        )
        .await
        .unwrap();

        // An Owner reads every Firm's awaiting rows; the decided row is gone.
        let owner_rows = list_for_review(&db, owner).await.unwrap();
        assert_eq!(owner_rows.len(), 2);
        assert!(owner_rows
            .iter()
            .all(|row| row.triage.disposition != TriageDisposition::Overridden));

        // A firm-tier member reads only their Firm's rows.
        let lawyer_rows = list_for_review(&db, lawyer).await.unwrap();
        assert_eq!(lawyer_rows.len(), 1);
        assert_eq!(lawyer_rows[0].triage.message_id, first.message_id);
        assert_eq!(lawyer_rows[0].channel, "email");

        // A Client is denied outright.
        let client = person(&db, "Queue Client", Role::Client).await;
        assert!(matches!(
            list_for_review(&db, client).await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn an_owner_confirms_the_suggested_project() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let owner = person(&db, "Confirm Owner", Role::Owner).await;
        let project = seed_project(&db, firm, "Confirm Target").await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "confirm-owner", Some(project.id)).await;

        let confirmed = confirm_project(&db, owner, triage_id, project.id)
            .await
            .unwrap();
        assert_eq!(confirmed.disposition, TriageDisposition::Confirmed);
        assert!(!confirmed.labeled_example);
        // The suggestion is now the conversation's assignment, recorded as
        // an auditable assignment row.
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, Some(project.id));
        assert_eq!(
            crate::inbox::assignments(&db, conversation.id)
                .await
                .unwrap()
                .len(),
            1
        );
        // The decision is recorded; a second confirm cannot rewrite it.
        let again = confirm_project(&db, owner, triage_id, project.id).await;
        assert!(matches!(again, Err(InboxError::Invalid(_))));
        assert!(
            list_for_review(&db, owner).await.unwrap().is_empty(),
            "a decided row leaves the queue"
        );
    }

    #[tokio::test]
    async fn confirm_takes_the_suggested_project_only() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let owner = person(&db, "Confirm Strict Owner", Role::Owner).await;
        let suggested = seed_project(&db, firm, "Suggested Matter").await;
        let other = seed_project(&db, firm, "Other Matter").await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "confirm-strict", Some(suggested.id)).await;

        let error = confirm_project(&db, owner, triage_id, other.id).await;
        assert!(matches!(error, Err(InboxError::Invalid(_))));
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, None);
    }

    #[tokio::test]
    async fn a_lawyer_confirms_only_while_participating() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let project = seed_project(&db, firm, "Participation Matter").await;
        let (outside_id, _) = needs_human_with_suggestion(&db, firm, "confirm-lawyer-out", None)
            .await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "confirm-lawyer", Some(project.id)).await;
        let lawyer = person(&db, "Firm Lawyer", Role::Lawyer).await;
        member(&db, lawyer, firm, FirmMembership::Lawyer).await;

        // Firm membership alone is not the participation check.
        let denied = confirm_project(&db, lawyer, triage_id, project.id).await;
        assert!(matches!(denied, Err(InboxError::Denied)));
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, None);

        // With active participation the same action lands.
        participate(&db, lawyer, project.id).await;
        confirm_project(&db, lawyer, triage_id, project.id)
            .await
            .unwrap();
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, Some(project.id));

        // An Admin acts without participation; a Client never acts.
        let admin = person(&db, "Firm Admin", Role::Admin).await;
        let admin_row = {
            let message = inbound(&db, firm, "email", "confirm-admin").await;
            record(
                &db,
                &new_triage(
                    message.message_id,
                    firm,
                    Some(project.id),
                    PrivilegeLabel::Uncertain,
                    TriageDisposition::NeedsHuman,
                ),
            )
            .await
            .unwrap()
        };
        confirm_project(&db, admin, admin_row.id, project.id)
            .await
            .unwrap();
        let client = person(&db, "Confirm Client", Role::Client).await;
        let (client_row, _) =
            needs_human_with_suggestion(&db, firm, "confirm-client", Some(project.id)).await;
        assert!(matches!(
            confirm_project(&db, client, client_row, project.id).await,
            Err(InboxError::Denied)
        ));
        assert!(matches!(
            confirm_project(&db, client, outside_id, project.id).await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn an_override_records_the_correction_as_a_labeled_example() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let owner = person(&db, "Override Owner", Role::Owner).await;
        let suggested = seed_project(&db, firm, "Wrong Suggestion").await;
        let corrected = seed_project(&db, firm, "Right Matter").await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "override-labeled", Some(suggested.id)).await;

        let overridden = override_project(
            &db,
            owner,
            triage_id,
            corrected.id,
            "the sender is on the Right Matter",
        )
        .await
        .unwrap();
        assert_eq!(overridden.disposition, TriageDisposition::Overridden);
        assert!(overridden.labeled_example);
        assert_eq!(overridden.suggested_project_id, Some(corrected.id));
        assert_eq!(overridden.project_confidence, None);
        assert!(overridden
            .rationale
            .as_deref()
            .unwrap_or_default()
            .starts_with("human override:"));
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, Some(corrected.id));

        // The correction crosses Firms at its own peril.
        let other = crate::test_support::seed_firm_with_brand(&db, "lawyer-shook").await;
        let foreign = seed_project(&db, other, "Foreign Matter").await;
        let (open_row, _) = needs_human_with_suggestion(&db, firm, "override-foreign", None).await;
        let error = override_project(&db, owner, open_row, foreign.id, "cross firm").await;
        assert!(matches!(error, Err(InboxError::CrossFirm)));
        // And an override carries a rationale.
        let (blank_row, _) =
            needs_human_with_suggestion(&db, firm, "override-blank", None).await;
        let error = override_project(&db, owner, blank_row, corrected.id, "  ").await;
        assert!(matches!(error, Err(InboxError::Invalid(_))));
    }

    #[tokio::test]
    async fn resolving_privilege_keeps_the_rule_layer_authoritative() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let owner = person(&db, "Resolve Owner", Role::Owner).await;

        // The human may settle client email either way.
        let (email_row, _) = needs_human_with_suggestion(&db, firm, "resolve-email", None).await;
        let resolved = resolve_privilege(
            &db,
            owner,
            email_row,
            PrivilegeLabel::Privileged,
            "the sender is a client discussing their trust",
        )
        .await
        .unwrap();
        assert_eq!(resolved.privilege, PrivilegeLabel::Privileged);
        assert_eq!(resolved.privilege_confidence, None);
        assert_eq!(resolved.disposition, TriageDisposition::NeedsHuman);
        assert!(resolved
            .rationale
            .as_deref()
            .unwrap_or_default()
            .starts_with("human review:"));

        // The human may call client email not privileged — a person can.
        let (email_two, _) = needs_human_with_suggestion(&db, firm, "resolve-email-two", None).await;
        let settled = resolve_privilege(
            &db,
            owner,
            email_two,
            PrivilegeLabel::NotPrivileged,
            "a newsletter, not a client",
        )
        .await
        .unwrap();
        assert_eq!(settled.privilege, PrivilegeLabel::NotPrivileged);

        // A social-origin message is not the human's to flag: the rule
        // layer forces the label back, whatever the form posts.
        let message = inbound(&db, firm, "instagram", "resolve-social").await;
        let social = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                None,
                PrivilegeLabel::NotPrivileged,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await
        .unwrap();
        let forced = resolve_privilege(
            &db,
            owner,
            social.id,
            PrivilegeLabel::Privileged,
            "this looks privileged to me",
        )
        .await
        .unwrap();
        assert_eq!(
            forced.privilege,
            PrivilegeLabel::NotPrivileged,
            "the structural rule outranks the form"
        );

        // Resolving to Uncertain is not a resolution; a resolution carries
        // a reason.
        let (open_row, _) = needs_human_with_suggestion(&db, firm, "resolve-open", None).await;
        assert!(matches!(
            resolve_privilege(&db, owner, open_row, PrivilegeLabel::Uncertain, "keep guessing")
                .await,
            Err(InboxError::Invalid(_))
        ));
        assert!(matches!(
            resolve_privilege(&db, owner, open_row, PrivilegeLabel::Privileged, "  ").await,
            Err(InboxError::Invalid(_))
        ));

        // A Client never resolves; an outside-the-Firm Lawyer cannot see
        // the row's Firm.
        let client = person(&db, "Resolve Client", Role::Client).await;
        assert!(matches!(
            resolve_privilege(&db, client, open_row, PrivilegeLabel::Privileged, "mine").await,
            Err(InboxError::Denied)
        ));
        let outsider = person(&db, "Resolve Outsider", Role::Lawyer).await;
        assert!(matches!(
            resolve_privilege(&db, outsider, open_row, PrivilegeLabel::Privileged, "not mine")
                .await,
            Err(InboxError::Denied)
        ));
    }

    #[tokio::test]
    async fn a_rerun_returns_a_resolved_row_to_the_queue() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let owner = person(&db, "Rerun Owner", Role::Owner).await;
        let project = seed_project(&db, firm, "Rerun Matter").await;

        // A confirmed email row reopens as Uncertain — the portal re-runs
        // the rule layer, and email without a classifier is uncertain.
        let (triage_id, _) = needs_human_with_suggestion(&db, firm, "rerun-email", Some(project.id))
            .await;
        confirm_project(&db, owner, triage_id, project.id).await.unwrap();
        let reopened = rerun(&db, owner, triage_id).await.unwrap();
        assert_eq!(reopened.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(reopened.disposition, TriageDisposition::NeedsHuman);
        assert_eq!(reopened.suggested_project_id, Some(project.id));
        assert!(
            list_for_review(&db, owner).await.unwrap().len() == 1,
            "the reopened row waits again"
        );

        // A labeled example survives the re-run.
        let overridden = override_project(&db, owner, triage_id, project.id, "still right").await.unwrap();
        assert!(overridden.labeled_example);
        let rerun_row = rerun(&db, owner, triage_id).await.unwrap();
        assert!(rerun_row.labeled_example);

        // A social row re-runs to the rule layer's settled label.
        let message = inbound(&db, firm, "threads", "rerun-social").await;
        let social = record(
            &db,
            &new_triage(
                message.message_id,
                firm,
                Some(project.id),
                PrivilegeLabel::NotPrivileged,
                TriageDisposition::Suggested,
            ),
        )
        .await
        .unwrap();
        let social_rerun = rerun(&db, owner, social.id).await.unwrap();
        assert_eq!(social_rerun.privilege, PrivilegeLabel::NotPrivileged);
        assert_eq!(social_rerun.disposition, TriageDisposition::Suggested);
    }
}
