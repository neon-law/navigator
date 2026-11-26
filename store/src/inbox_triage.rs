//! The triage row for one inbound Message: whether it is privileged, which
//! Project is suggested for it, and the audit trail of how both were
//! decided.
//!
//! The privilege label is structural, not statistical: a social-origin
//! public post is not privileged; private or unknown visibility waits for human
//! review. Client email may only be flagged
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
    /// A confident client-email classifier flag, or a human review decision.
    #[serde(alias = "privileged")]
    Privileged,
    /// Explicitly public social posts, or a human review decision.
    #[serde(alias = "not_privileged")]
    NotPrivileged,
    /// Everything automation cannot settle — including client email below
    /// the confidence floor. These rows wait for a person.
    #[serde(alias = "uncertain")]
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
    #[serde(alias = "suggested")]
    Suggested,
    /// A person confirmed the suggested Project.
    #[serde(alias = "confirmed")]
    Confirmed,
    /// A person corrected the suggestion; the row is a labeled example for
    /// the next classifier generation.
    #[serde(alias = "overridden")]
    Overridden,
    /// Uncertain privilege or no confident Project — the row is in the
    /// human review queue.
    #[serde(alias = "needs_human")]
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
    let conversation = conversation_for_message(db, triage.message_id).await?;
    if conversation.firm_id != triage.firm_id {
        return Err(InboxError::CrossFirm);
    }
    let message = crate::inbox::message_by_id(db, triage.message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let mut triage = triage.clone();
    if crate::inbox::is_social_channel(&conversation.channel)
        && (message.visibility.as_deref() != Some("public")
            || conversation.interaction_type == "direct_message")
    {
        triage.privilege = PrivilegeLabel::Uncertain;
        triage.privilege_confidence = None;
        triage.project_confidence = None;
        triage.disposition = TriageDisposition::NeedsHuman;
        triage.model_id = None;
        triage.prompt_version = RULES_ONLY_PROMPT.into();
        triage.raw_output = None;
        triage.rationale = None;
        triage.labeled_example = false;
    }
    if let Some(project_id) = triage.suggested_project_id {
        let project = crate::projects::find_by_id(db, project_id)
            .await?
            .ok_or(InboxError::NotFound)?;
        if project.firm_id != Some(triage.firm_id) {
            return Err(InboxError::CrossFirm);
        }
    }
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(triage_pg_error)?
    {
        let now = chrono::Utc::now();
        return crate::postgres::matter::upsert(&pg, TABLE, &serde_json::json!({
            "id": Uuid::now_v7(), "message_id": triage.message_id, "firm_id": triage.firm_id,
            "suggested_project_id": triage.suggested_project_id, "project_confidence": triage.project_confidence,
            "privilege": triage.privilege.as_str(), "privilege_confidence": triage.privilege_confidence,
            "rationale": triage.rationale,"model_id": triage.model_id,"prompt_version": triage.prompt_version,
            "raw_output": triage.raw_output,"disposition": triage.disposition.as_str(),
            "labeled_example": triage.labeled_example,"inserted_at": now,"updated_at": now,
        })).await.map_err(triage_pg_error);
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
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(triage_pg_error)?
    {
        return crate::postgres::matter::find(
            &pg,
            TABLE,
            serde_json::json!({"message_id": message_id}),
        )
        .await
        .map_err(triage_pg_error);
    }

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
    matches!(
        channel,
        crate::inbox::channel::EMAIL | crate::inbox::channel::SMS
    )
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
/// Explicitly public social posts are not privileged. Private messages and
/// unknown visibility remain uncertain and never consult a classifier.
/// Client email and SMS require a confident privileged classifier verdict;
/// other automated outcomes remain uncertain until human review.
#[must_use]
pub fn privilege_label(
    channel: &str,
    public_social: bool,
    classifier: Option<&ClassifierOutput>,
) -> PrivilegeLabel {
    if crate::inbox::is_social_channel(channel) {
        return if public_social {
            PrivilegeLabel::NotPrivileged
        } else {
            PrivilegeLabel::Uncertain
        };
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
    let rows: Vec<InboxTriage> = if let Some(pg) = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(triage_pg_error)?
    {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(r) FROM inbox_triage r WHERE disposition IN ('needs_human','suggested') ORDER BY updated_at DESC,id DESC")
            .fetch_all(pg.pool()).await.map_err(crate::postgres::matter::MatterAuthorityError::from).map_err(triage_pg_error)?;
        rows.into_iter()
            .map(|row| {
                serde_json::from_value(row)
                    .map_err(crate::postgres::matter::MatterAuthorityError::from)
                    .map_err(triage_pg_error)
            })
            .collect::<Result<_, _>>()?
    } else {
        let mut response = db.query(format!("SELECT {SELECT} FROM {TABLE} WHERE disposition = 'needs_human' OR disposition = 'suggested' ORDER BY updated_at DESC"))
            .await?.check()?;
        let rows: Vec<TriageRow> = response.take(0)?;
        rows.into_iter()
            .filter_map(TriageRow::into_triage)
            .collect()
    };
    let mut review_rows = Vec::new();
    for triage in rows {
        if let Some(firms) = &visible_firms {
            if !firms.contains(&triage.firm_id) {
                continue;
            }
        }
        let Some(row) = review_row(db, &actor, triage).await? else {
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
    actor: &crate::persons::Person,
    mut triage: InboxTriage,
) -> Result<Option<TriageReviewRow>, InboxError> {
    let Ok(conversation) = conversation_for_message(db, triage.message_id).await else {
        // A message whose conversation vanished leaves no queue entry to
        // act on — the row is orphaned, not a routing decision.
        return Ok(None);
    };
    if let Some(project_id) = conversation.project_id {
        if !crate::access::can_see_project(db, Some(actor.id), actor.role, project_id)
            .await
            .map_err(InboxError::Db)?
        {
            triage.rationale = None;
            triage.raw_output = None;
        }
    }
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
pub async fn find_by_id(
    db: &SurrealDb,
    triage_id: Uuid,
) -> Result<Option<InboxTriage>, InboxError> {
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(triage_pg_error)?
    {
        return crate::postgres::matter::find(&pg, TABLE, serde_json::json!({"id": triage_id}))
            .await
            .map_err(triage_pg_error);
    }

    let mut response = db
        .query(format!("SELECT {SELECT} FROM $id"))
        .bind(("id", record_id(TABLE, triage_id)))
        .await?
        .check()?;
    let rows: Vec<TriageRow> = response.take(0)?;
    Ok(rows.into_iter().find_map(TriageRow::into_triage))
}

/// Identity carried by one lawyer-approved Message filing. The workflow
/// needs only these references to recheck the decision after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingApproval {
    pub triage_id: Uuid,
    pub message_id: Uuid,
    pub project_id: Uuid,
    pub approved_by: Uuid,
    pub review_revision: chrono::DateTime<chrono::Utc>,
    pub assignment_id: Option<Uuid>,
}

async fn latest_assignment_id(
    db: &SurrealDb,
    conversation_id: Uuid,
) -> Result<Option<Uuid>, InboxError> {
    Ok(crate::inbox::assignments(db, conversation_id)
        .await?
        .into_iter()
        .max_by_key(|assignment| (assignment.inserted_at, assignment.id))
        .map(|assignment| assignment.id))
}

/// Prepare the exact Message and review revision to show a reviewer. The
/// browser still has to submit approval and the worker rechecks authority.
///
/// # Errors
///
/// Propagates store errors. Returns no action for an inaccessible or stale proposal.
pub async fn filing_action_for_review(
    db: &SurrealDb,
    actor_id: Uuid,
    row: &TriageReviewRow,
) -> Result<Option<FilingApproval>, InboxError> {
    let Some(project_id) = row.triage.suggested_project_id else {
        return Ok(None);
    };
    let approval = FilingApproval {
        triage_id: row.triage.id,
        message_id: row.triage.message_id,
        project_id,
        approved_by: actor_id,
        review_revision: row.triage.updated_at,
        assignment_id: latest_assignment_id(db, row.conversation_id).await?,
    };
    match verify_filing_approval(db, &approval).await {
        Ok(()) => Ok(Some(approval)),
        Err(InboxError::Denied | InboxError::NotFound) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn approved_filing_context(
    db: &SurrealDb,
    approval: &FilingApproval,
) -> Result<(crate::inbox::Conversation, crate::inbox::Message), InboxError> {
    let actor = crate::inbox::load_person(db, approval.approved_by).await?;
    if !crate::access::matter_viewer(db, Some(actor.id), actor.role, approval.project_id)
        .await
        .map_err(InboxError::Db)?
        .is_some_and(crate::access::MatterViewer::is_firm_side)
    {
        return Err(InboxError::Denied);
    }
    let triage = find_by_id(db, approval.triage_id)
        .await?
        .filter(|triage| {
            triage.message_id == approval.message_id
                && triage.suggested_project_id == Some(approval.project_id)
                && triage.updated_at == approval.review_revision
        })
        .ok_or(InboxError::NotFound)?;
    if !crate::inbox::triage_reviewer_may_resolve(db, &actor, triage.firm_id).await? {
        return Err(InboxError::Denied);
    }
    let project = crate::projects::find_by_id(db, approval.project_id)
        .await?
        .filter(|project| project.firm_id == Some(triage.firm_id) && project.status == "open")
        .ok_or(InboxError::NotFound)?;
    let message = crate::inbox::message_by_id(db, approval.message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = crate::inbox::conversation_by_id(db, message.conversation_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !filing_source_is_reviewable(&conversation, &message) {
        return Err(InboxError::NotFound);
    }
    if conversation.firm_id != triage.firm_id
        || conversation.trashed_at.is_some()
        || conversation
            .brand_key
            .as_deref()
            .is_some_and(|brand| brand != project.brand)
        || conversation
            .project_id
            .is_some_and(|assigned| assigned != project.id)
    {
        return Err(InboxError::NotFound);
    }
    if latest_assignment_id(db, conversation.id).await? != approval.assignment_id {
        return Err(InboxError::NotFound);
    }
    Ok((conversation, message))
}

fn filing_source_is_reviewable(
    conversation: &crate::inbox::Conversation,
    message: &crate::inbox::Message,
) -> bool {
    if message.direction == crate::inbox::direction::INBOUND {
        return true;
    }
    // Imported sent mail is already delivered. Inbox-created drafts and
    // outbound sends have no historical archive and cannot enter this lane.
    conversation.channel == crate::inbox::channel::EMAIL
        && message.direction == crate::inbox::direction::OUTBOUND
        && message.delivery_state == crate::inbox::delivery::RECEIVED
        && message.author_person_id.is_some()
        && message
            .raw_storage_key
            .as_deref()
            .and_then(|key| key.strip_prefix("mail/")?.split_once('.'))
            .is_some_and(|(digest, extension)| {
                digest.len() == 64
                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && extension.eq_ignore_ascii_case("eml")
            })
}

/// Recheck a filing submitted by its authenticated reviewer before queuing it.
///
/// # Errors
///
/// Returns a denial, a stale-proposal error or a store error.
pub async fn verify_filing_approval(
    db: &SurrealDb,
    approval: &FilingApproval,
) -> Result<(), InboxError> {
    approved_filing_context(db, approval).await.map(|_| ())
}

/// The exact Inbox Message Asset and document kind shown before approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentFilingApproval {
    pub filing: FilingApproval,
    pub attachment_asset_id: Uuid,
    pub attachment_position: i64,
    pub kind: String,
}

/// Metadata an attachment worker may load after rechecking the review.
#[derive(Debug)]
pub struct ApprovedAttachment {
    pub source: crate::inbox::MessageAttachment,
    pub sender: String,
    pub subject: Option<String>,
    pub occurred_at: String,
}

/// Resolve the exact attachment attached to the approved Message and validate
/// the reviewer-selected document kind before any worker reads object bytes.
///
/// # Errors
///
/// Denies stale approvals, unrelated assets, and unsupported document kinds.
pub async fn approved_attachment(
    db: &SurrealDb,
    approval: &AttachmentFilingApproval,
) -> Result<ApprovedAttachment, InboxError> {
    if !rules::kind::Kind::parse(&approval.kind)
        .is_some_and(|kind| kind.valid_for(rules::kind::Lane::Asset))
    {
        return Err(InboxError::Denied);
    }
    let (conversation, message) = approved_filing_context(db, &approval.filing).await?;
    let source = crate::inbox::message_assets(db, conversation.firm_id, message.id)
        .await?
        .into_iter()
        .find(|source| {
            source.asset_id == approval.attachment_asset_id
                && source.position == approval.attachment_position
        })
        .ok_or(InboxError::NotFound)?;
    Ok(ApprovedAttachment {
        source,
        sender: conversation.external_address,
        subject: conversation.subject,
        occurred_at: message.occurred_at,
    })
}

/// Persist the one authorized attachment action before worker submission.
///
/// # Errors
///
/// Returns an error for stale approvals, missing PostgreSQL authority or storage errors.
pub async fn queue_attachment_filing(
    db: &SurrealDb,
    approval: &AttachmentFilingApproval,
    retry: bool,
) -> Result<Uuid, InboxError> {
    approved_attachment(db, approval).await?;
    let pg = crate::inbox::inbox_authority()
        .await?
        .ok_or(InboxError::PostgresAuthorityRequired)?;
    sqlx::query_scalar(
        "INSERT INTO inbox_attachment_filing (id, message_id, project_id, attachment_asset_id, approval)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (approval) DO UPDATE SET
             attempt = CASE WHEN $6 AND inbox_attachment_filing.failure_code = 'unavailable'
                            THEN inbox_attachment_filing.attempt + 1 ELSE inbox_attachment_filing.attempt END,
             state = CASE WHEN $6 AND inbox_attachment_filing.failure_code = 'unavailable'
                          THEN 'pending' ELSE inbox_attachment_filing.state END,
             failure_code = CASE WHEN $6 AND inbox_attachment_filing.failure_code = 'unavailable'
                                 THEN NULL ELSE inbox_attachment_filing.failure_code END
         RETURNING inbox_attachment_filing.id",
    )
    .bind(Uuid::now_v7())
    .bind(approval.filing.message_id)
    .bind(approval.filing.project_id)
    .bind(approval.attachment_asset_id)
    .bind(sqlx::types::Json(approval))
    .bind(retry)
    .fetch_one(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))
}

/// The latest job outcome for one attachment under the current approval scope.
#[derive(Debug)]
pub struct AttachmentFilingStatus {
    pub asset_id: Uuid,
    pub kind: String,
    pub state: String,
    pub failure_code: Option<String>,
}

/// Look up this reviewer's own attachment outcomes for the current revision.
///
/// # Errors
///
/// Returns an error if the approval scope has changed or the database is unavailable.
pub async fn attachment_filing_statuses(
    db: &SurrealDb,
    filing: &FilingApproval,
) -> Result<Vec<AttachmentFilingStatus>, InboxError> {
    verify_filing_approval(db, filing).await?;
    let pg = crate::inbox::inbox_authority()
        .await?
        .ok_or(InboxError::PostgresAuthorityRequired)?;
    let rows: Vec<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (attachment_asset_id) attachment_asset_id AS asset_id,
                approval->>'kind' AS kind, state, failure_code
         FROM inbox_attachment_filing WHERE message_id = $1 AND project_id = $2
           AND approval->'filing' = $3
         ORDER BY attachment_asset_id, inserted_at DESC, id DESC",
    )
    .bind(filing.message_id)
    .bind(filing.project_id)
    .bind(sqlx::types::Json(filing))
    .fetch_all(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(
            |(asset_id, kind, state, failure_code)| AttachmentFilingStatus {
                asset_id,
                kind,
                state,
                failure_code,
            },
        )
        .collect())
}

/// The current attempt on an approved attachment filing job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentFilingAttempt {
    pub id: Uuid,
    pub attempt: i32,
}

/// List the oldest pending attachment filing attempts for reconciliation.
///
/// # Errors
///
/// Returns an error if PostgreSQL is unavailable.
pub async fn pending_attachment_filings(
    limit: i64,
) -> Result<Vec<AttachmentFilingAttempt>, InboxError> {
    let pg = crate::inbox::inbox_authority()
        .await?
        .ok_or(InboxError::PostgresAuthorityRequired)?;
    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT id, attempt FROM inbox_attachment_filing
         WHERE state = 'pending' ORDER BY inserted_at, id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(rows
        .into_iter()
        .map(|(id, attempt)| AttachmentFilingAttempt { id, attempt })
        .collect())
}

/// Load only the opaque worker input for a pending attachment filing.
///
/// # Errors
///
/// Propagates PostgreSQL failures.
pub async fn pending_attachment_filing(
    job: AttachmentFilingAttempt,
) -> Result<Option<AttachmentFilingApproval>, InboxError> {
    let pg = crate::inbox::inbox_authority()
        .await?
        .ok_or(InboxError::PostgresAuthorityRequired)?;
    let row: Option<sqlx::types::Json<AttachmentFilingApproval>> = sqlx::query_scalar(
        "SELECT approval FROM inbox_attachment_filing WHERE id = $1 AND attempt = $2 AND state = 'pending'",
    )
    .bind(job.id)
    .bind(job.attempt)
    .fetch_optional(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(row.map(|json| json.0))
}

/// Record a terminal attachment filing outcome once.
///
/// # Errors
///
/// Propagates PostgreSQL failures.
pub async fn finish_attachment_filing(
    job: AttachmentFilingAttempt,
    result: Result<Uuid, &'static str>,
) -> Result<(), InboxError> {
    let pg = crate::inbox::inbox_authority()
        .await?
        .ok_or(InboxError::PostgresAuthorityRequired)?;
    let (state, asset, failure) = match result {
        Ok(asset) => ("filed", Some(asset), None),
        Err(code) => ("failed", None, Some(code)),
    };
    sqlx::query(
        "UPDATE inbox_attachment_filing SET state = $3, result_asset_id = $4, failure_code = $5
         WHERE id = $1 AND attempt = $2 AND state = 'pending'",
    )
    .bind(job.id)
    .bind(job.attempt)
    .bind(state)
    .bind(asset)
    .bind(failure)
    .execute(pg.pool())
    .await
    .map_err(|error| InboxError::Db(error.to_string()))?;
    Ok(())
}

/// File the exact approved inbound Message as a firm-internal Communication.
/// Recheck the review revision, assignment and approver's current Firm and
/// Project access. The normal Inbox source reference makes replay idempotent.
/// The same approval also allows filing a historical copy of sent email.
///
/// # Errors
///
/// Denies an approver who lost access, or a stale approval. Propagates store
/// failures and source references that already belong to another Project.
pub async fn file_approved_message(
    db: &SurrealDb,
    approval: &FilingApproval,
) -> Result<Uuid, InboxError> {
    let (conversation, message) = approved_filing_context(db, approval).await?;
    let channel = crate::inbox::spine_channel(&conversation.channel, &message.direction);
    if channel.is_empty() {
        return Err(InboxError::Denied);
    }
    let source_ref = format!("inbox:{}", message.id);
    let result = crate::communications::ingest(
        db,
        &crate::communications::IngestArgs {
            project_id: approval.project_id,
            channel,
            direction: crate::communications::direction::INTERNAL,
            author_person_id: message.author_person_id,
            counterparty: Some(&conversation.external_address),
            subject: conversation.subject.as_deref(),
            body: &message.body,
            source_ref: Some(&source_ref),
            asset_id: None,
            occurred_at: &message.occurred_at,
        },
    )
    .await?;
    Ok(result.communication_id)
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
    if let Some(pg) = crate::postgres::matter::authority_for(TABLE)
        .await
        .map_err(triage_pg_error)?
    {
        return crate::postgres::matter::update(&pg, TABLE, serde_json::json!({"id": triage_id}), serde_json::json!({
            "privilege": decision.privilege.as_str(),"privilege_confidence": decision.privilege_confidence,
            "suggested_project_id": decision.suggested_project_id,"project_confidence": decision.project_confidence,
            "rationale": decision.rationale,"disposition": decision.disposition.as_str(),
            "labeled_example": decision.labeled_example,"updated_at": chrono::Utc::now(),
        })).await.map_err(triage_pg_error)?.into_iter().next().ok_or(InboxError::NotFound);
    }

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

/// Record a Project choice made in the Inbox review queue. Email choices
/// leave each Message available for its own explicit filing approval.
///
/// # Errors
///
/// Returns an error when the review or Project is out of scope, stale, or invalid.
pub async fn review_queue_project(
    db: &SurrealDb,
    actor_id: Uuid,
    triage_id: Uuid,
    project_id: Uuid,
    correction: Option<&str>,
) -> Result<InboxTriage, InboxError> {
    let row = find_by_id(db, triage_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let conversation = crate::inbox::conversation_by_id(db, row_conversation_id(db, &row).await?)
        .await?
        .ok_or(InboxError::NotFound)?;
    if conversation.channel != crate::inbox::channel::EMAIL {
        return match correction {
            Some(rationale) => {
                override_project(db, actor_id, triage_id, project_id, rationale).await
            }
            None => confirm_project(db, actor_id, triage_id, project_id).await,
        };
    }
    review_email_project(db, actor_id, &row, &conversation, project_id, correction).await
}

async fn review_email_project(
    db: &SurrealDb,
    actor_id: Uuid,
    row: &InboxTriage,
    conversation: &crate::inbox::Conversation,
    project_id: Uuid,
    correction: Option<&str>,
) -> Result<InboxTriage, InboxError> {
    require_open(row)?;
    crate::inbox::require_triage_reviewer(db, actor_id, project_id).await?;
    let project = crate::projects::find_by_id(db, project_id)
        .await?
        .filter(|project| project.status == "open")
        .ok_or(InboxError::NotFound)?;
    if project.firm_id != Some(row.firm_id) || conversation.firm_id != row.firm_id {
        return Err(InboxError::CrossFirm);
    }
    if conversation.trashed_at.is_some()
        || conversation
            .brand_key
            .as_deref()
            .is_some_and(|brand| brand != project.brand)
        || conversation
            .project_id
            .is_some_and(|assigned| assigned != project.id)
    {
        return Err(InboxError::NotFound);
    }
    let Some(rationale) = correction else {
        return if row.suggested_project_id == Some(project.id) {
            Ok(row.clone())
        } else {
            Err(InboxError::Invalid(
                "choose the current Project proposal or enter a correction".into(),
            ))
        };
    };
    let rationale = rationale.trim();
    if rationale.is_empty() {
        return Err(InboxError::Invalid(
            "a correction carries a rationale".into(),
        ));
    }
    write_review_decision(
        db,
        row.id,
        ReviewDecision {
            privilege: row.privilege,
            privilege_confidence: row.privilege_confidence,
            suggested_project_id: Some(project.id),
            project_confidence: None,
            rationale: Some(format!("human override: {rationale}")),
            disposition: TriageDisposition::NeedsHuman,
            labeled_example: true,
        },
    )
    .await
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
    let row = find_by_id(db, triage_id)
        .await?
        .ok_or(InboxError::NotFound)?;
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
    let conversation_id = row_conversation_id(db, &row).await?;
    if !crate::inbox::assignments(db, conversation_id)
        .await?
        .is_empty()
        && row.privilege == PrivilegeLabel::Uncertain
    {
        return Err(InboxError::Invalid(
            "resolve privilege before changing the attribution again".into(),
        ));
    }
    let assignment =
        crate::inbox::assign_for_triage_review(db, actor_id, conversation_id, project_id).await?;
    write_review_decision(
        db,
        triage_id,
        ReviewDecision {
            privilege: row.privilege,
            privilege_confidence: row.privilege_confidence,
            suggested_project_id: Some(assignment.to_project_id),
            project_confidence: row.project_confidence,
            rationale: row.rationale.clone(),
            disposition: if row.privilege == PrivilegeLabel::Uncertain {
                TriageDisposition::NeedsHuman
            } else {
                TriageDisposition::Confirmed
            },
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
        return Err(InboxError::Invalid(
            "an override carries a rationale".into(),
        ));
    }
    let row = find_by_id(db, triage_id)
        .await?
        .ok_or(InboxError::NotFound)?;
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
            disposition: if row.privilege == PrivilegeLabel::Uncertain {
                TriageDisposition::NeedsHuman
            } else {
                TriageDisposition::Overridden
            },
            labeled_example: true,
        },
    )
    .await
}

/// A person settles an uncertain privilege label. The rule layer stays
/// authoritative on both sides of the form: an explicitly public social post is
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
        return Err(InboxError::Invalid(
            "a resolution carries a rationale".into(),
        ));
    }
    let row = find_by_id(db, triage_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    require_open(&row)?;
    let actor = crate::persons::find_by_id(db, actor_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !crate::inbox::triage_reviewer_may_resolve(db, &actor, row.firm_id).await? {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_for_message(db, row.message_id).await?;
    if let Some(project_id) = conversation.project_id {
        if !crate::access::can_see_project(db, Some(actor.id), actor.role, project_id)
            .await
            .map_err(InboxError::Db)?
        {
            return Err(InboxError::Denied);
        }
    }
    let message = crate::inbox::message_by_id(db, row.message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let public_social = message.visibility.as_deref() == Some("public")
        && conversation.interaction_type != "direct_message";
    // Public posts retain the structural rule; private messages accept human review.
    let privilege = if crate::inbox::is_social_channel(&conversation.channel) && public_social {
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
            rationale: Some(
                match row.rationale.as_deref().filter(|_| row.labeled_example) {
                    Some(previous) => format!("human review: {rationale}\n{previous}"),
                    None => format!("human review: {rationale}"),
                },
            ),
            disposition: if crate::inbox::assignments(db, conversation.id)
                .await?
                .is_empty()
            {
                automated_disposition(privilege, row.suggested_project_id)
            } else if row.labeled_example {
                TriageDisposition::Overridden
            } else {
                TriageDisposition::Confirmed
            },
            labeled_example: row.labeled_example,
        },
    )
    .await
}

/// A fresh pass of the rule layer over a triage row: the label is
/// recomputed from channel and visibility: public social posts land
/// `NotPrivileged`; private social messages, client email and SMS land `Uncertain` (the
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
    let row = find_by_id(db, triage_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let actor = crate::persons::find_by_id(db, actor_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    if !crate::inbox::triage_reviewer_may_resolve(db, &actor, row.firm_id).await? {
        return Err(InboxError::Denied);
    }
    let conversation = conversation_for_message(db, row.message_id).await?;
    if let Some(project_id) = conversation.project_id {
        if !crate::access::can_see_project(db, Some(actor.id), actor.role, project_id)
            .await
            .map_err(InboxError::Db)?
        {
            return Err(InboxError::Denied);
        }
    }
    let message = crate::inbox::message_by_id(db, row.message_id)
        .await?
        .ok_or(InboxError::NotFound)?;
    let public_social = message.visibility.as_deref() == Some("public")
        && conversation.interaction_type != "direct_message";
    let privilege = privilege_label(&conversation.channel, public_social, None);
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
async fn row_conversation_id(db: &SurrealDb, row: &InboxTriage) -> Result<Uuid, InboxError> {
    Ok(conversation_for_message(db, row.message_id).await?.id)
}

fn triage_pg_error(_error: crate::postgres::matter::MatterAuthorityError) -> InboxError {
    InboxError::Db("PostgreSQL triage authority is unavailable".into())
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
        for channel in ["facebook", "instagram", "threads", "youtube", "tiktok", "x"] {
            for verdict in [
                Some(classifier(true, 1.0)),
                Some(classifier(false, 1.0)),
                Some(classifier(true, 0.0)),
                None,
            ] {
                assert_eq!(
                    privilege_label(channel, true, verdict.as_ref()),
                    PrivilegeLabel::NotPrivileged,
                    "{channel} must settle privilege from the channel alone"
                );
            }
        }
    }

    #[test]
    fn hosted_classifier_is_limited_to_email_and_sms() {
        for channel in ["email", "sms"] {
            assert!(super::classifier_may_run(channel));
        }
        for channel in ["x", "instagram", "linkedin", "voice", "scraper", "unknown"] {
            assert!(
                !super::classifier_may_run(channel),
                "{channel} must not spend a model call"
            );
        }
    }

    #[test]
    fn private_or_unknown_social_visibility_requires_a_person_even_with_a_classifier() {
        for channel in ["x", "instagram", "facebook", "threads", "youtube", "tiktok"] {
            assert!(!super::classifier_may_run(channel));
            let label = privilege_label(channel, false, Some(&classifier(true, 1.0)));
            assert_eq!(label, PrivilegeLabel::Uncertain);
            assert_eq!(
                automated_disposition(label, Some(Uuid::now_v7())),
                TriageDisposition::NeedsHuman
            );
        }
    }

    #[test]
    fn client_email_at_or_above_the_floor_is_flagged() {
        assert_eq!(
            privilege_label("email", true, Some(&classifier(true, 0.90))),
            PrivilegeLabel::Privileged
        );
        assert_eq!(
            privilege_label("sms", true, Some(&classifier(true, 0.99))),
            PrivilegeLabel::Privileged
        );
    }

    #[test]
    fn client_email_below_the_floor_is_uncertain() {
        assert_eq!(
            privilege_label("email", true, Some(&classifier(true, 0.89))),
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
                    privilege_label(channel, true, verdict.as_ref()),
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
                visibility: Some("public"),
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
                attachments: Vec::new(),
                project_id: None,
                occurred_at: "2026-10-20T00:00:00Z",
                raw_storage_key: None,
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
    macro_rules! with_native_triage_fixture {
        ($db:ident, $pg:ident, $body:block) => {
            crate::test_support::with_postgres(async {
                let pg = crate::postgres::PostgresDb::application()
                    .await
                    .expect("required PostgreSQL application fixture");
                #[allow(unused_variables)]
                let $pg = pg.clone();
                crate::postgres::PostgresDb::with_application_db(pg, async move {
                    let $db = crate::surreal::SurrealDb::uninitialized();
                    $body
                })
                .await
                .expect("native PostgreSQL application fixture is admitted");
            })
            .await
        };
    }

    #[tokio::test]
    async fn unclassified_email_lands_needs_human() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let message = inbound(&db, firm, "email", "triage-uncertain").await;
        let label = privilege_label("email", true, None);
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

        });
    }

    /// A social-origin message settles at the rule layer: NotPrivileged with
    /// a suggested Project, and no model fields.
    #[tokio::test]
    async fn social_triage_settles_without_a_model() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let message = inbound(&db, firm, "instagram", "triage-social").await;
        let label = privilege_label("instagram", true, None);
        assert_eq!(label, PrivilegeLabel::NotPrivileged);
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Synthetic Triage Project")
                .await;
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

        });
    }

    /// A suggested Project from another Firm is a tenancy violation, not a
    /// suggestion — refused before the row exists.
    #[tokio::test]
    async fn cross_firm_project_suggestions_are_refused() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
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
        let project = crate::test_support::seed_project_on_firm(
            &db,
            Some(other.id),
            "Synthetic Other Firm Project",
        )
        .await;
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

        });
    }

    /// One Message has one triage row: a replayed write is refused and the
    /// first decision stands.
    #[tokio::test]
    async fn a_replayed_triage_does_not_overwrite_the_first() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
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

        });
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
                    name.to_lowercase().replace(' ', "-"),
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
        let role = crate::persons::find_by_id(db, person_id)
            .await
            .unwrap()
            .unwrap()
            .role;
        crate::projects::add_participation(
            db,
            project_id,
            person_id,
            crate::projects::participation_for_role(role),
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
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let other = crate::test_support::seed_firm_with_new_brand(&db, "summons").await.0;
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

        });
    }

    #[tokio::test]
    async fn an_owner_confirms_the_suggested_project() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Confirm Owner", Role::Owner).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Confirm Target").await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "confirm-owner", Some(project.id)).await;

        participate(&db, owner, project.id).await;
        let review = find_by_id(&db, triage_id).await.unwrap().unwrap();
        let approval = super::FilingApproval {
            triage_id,
            message_id: message.message_id,
            project_id: project.id,
            approved_by: owner,
            review_revision: review.updated_at,
            assignment_id: None,
        };
        let communication = super::file_approved_message(&db, &approval).await.unwrap();
        assert_eq!(
            super::file_approved_message(&db, &approval).await.unwrap(),
            communication
        );
        assert_eq!(
            crate::communications::for_project(&db, project.id)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            crate::communications::for_project_client_visible(&db, project.id)
                .await
                .unwrap()
                .is_empty()
        );

        let confirmed = confirm_project(&db, owner, triage_id, project.id)
            .await
            .unwrap();
        assert_eq!(confirmed.disposition, TriageDisposition::NeedsHuman);
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
        assert!(matches!(
            super::file_approved_message(&db, &approval).await,
            Err(InboxError::NotFound)
        ));
        let history = crate::inbox::assignments(&db, conversation.id)
            .await
            .unwrap();
        let renewed = super::FilingApproval {
            review_revision: confirmed.updated_at,
            assignment_id: Some(history[0].id),
            ..approval
        };
        assert_eq!(
            super::file_approved_message(&db, &renewed).await.unwrap(),
            communication
        );
        let wrong_message = super::FilingApproval {
            message_id: Uuid::now_v7(),
            ..renewed
        };
        assert!(matches!(
            super::file_approved_message(&db, &wrong_message).await,
            Err(InboxError::NotFound)
        ));
        // The decision is recorded; a second confirm cannot rewrite it.
        let again = confirm_project(&db, owner, triage_id, project.id).await;
        assert!(matches!(again, Err(InboxError::Invalid(_))));
        assert!(
            list_for_review(&db, owner).await.unwrap().len() == 1,
            "an attributed row still awaits privilege review"
        );

        });
    }

    #[tokio::test]
    async fn confirm_takes_the_suggested_project_only() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Confirm Strict Owner", Role::Owner).await;
        let suggested =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Suggested Matter").await;
        let other =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Other Matter").await;
        let (triage_id, message) =
            needs_human_with_suggestion(&db, firm, "confirm-strict", Some(suggested.id)).await;

        let error = confirm_project(&db, owner, triage_id, other.id).await;
        assert!(matches!(error, Err(InboxError::Invalid(_))));
        let conversation = conversation_for_message(&db, message.message_id)
            .await
            .unwrap();
        assert_eq!(conversation.project_id, None);

        });
    }

    #[tokio::test]
    async fn a_lawyer_confirms_only_while_participating() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Participation Matter")
                .await;
        let (outside_id, _) =
            needs_human_with_suggestion(&db, firm, "confirm-lawyer-out", None).await;
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
        member(&db, admin, firm, FirmMembership::Admin).await;
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

        });
    }

    #[tokio::test]
    async fn an_override_records_the_correction_as_a_labeled_example() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Override Owner", Role::Owner).await;
        let suggested =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Wrong Suggestion").await;
        let corrected =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Right Matter").await;
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
        assert_eq!(overridden.disposition, TriageDisposition::NeedsHuman);
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
        let other = crate::test_support::seed_firm_with_new_brand(&db, "summons").await.0;
        let foreign =
            crate::test_support::seed_project_on_firm(&db, Some(other), "Foreign Matter").await;
        let (open_row, _) = needs_human_with_suggestion(&db, firm, "override-foreign", None).await;
        let error = override_project(&db, owner, open_row, foreign.id, "cross firm").await;
        assert!(matches!(error, Err(InboxError::CrossFirm)));
        // And an override carries a rationale.
        let (blank_row, _) = needs_human_with_suggestion(&db, firm, "override-blank", None).await;
        let error = override_project(&db, owner, blank_row, corrected.id, "  ").await;
        assert!(matches!(error, Err(InboxError::Invalid(_))));

        });
    }

    #[tokio::test]
    async fn resolving_privilege_keeps_the_rule_layer_authoritative() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
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
        let (email_two, _) =
            needs_human_with_suggestion(&db, firm, "resolve-email-two", None).await;
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
            resolve_privilege(
                &db,
                owner,
                open_row,
                PrivilegeLabel::Uncertain,
                "keep guessing"
            )
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
            resolve_privilege(
                &db,
                outsider,
                open_row,
                PrivilegeLabel::Privileged,
                "not mine"
            )
            .await,
            Err(InboxError::Denied)
        ));

        });
    }

    #[tokio::test]
    async fn a_private_social_message_waits_for_privilege_review_after_assignment() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Private Review Owner", Role::Owner).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Private Review Matter")
                .await;
        participate(&db, owner, project.id).await;
        let inbound = inbound(&db, firm, "x", "private-review").await;
        sqlx::query("UPDATE message SET visibility = 'private' WHERE id = $1")
            .bind(inbound.message_id)
            .execute(pg.pool())
            .await
            .unwrap();
        let row = record(
            &db,
            &new_triage(
                inbound.message_id,
                firm,
                Some(project.id),
                PrivilegeLabel::Uncertain,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await
        .unwrap();
        let assigned = confirm_project(&db, owner, row.id, project.id)
            .await
            .unwrap();
        assert_eq!(assigned.disposition, TriageDisposition::NeedsHuman);
        assert_eq!(list_for_review(&db, owner).await.unwrap().len(), 1);
        let settled = resolve_privilege(
            &db,
            owner,
            row.id,
            PrivilegeLabel::Privileged,
            "private request for legal advice",
        )
        .await
        .unwrap();
        assert_eq!(settled.privilege, PrivilegeLabel::Privileged);
        assert_eq!(settled.disposition, TriageDisposition::Confirmed);
        assert!(list_for_review(&db, owner).await.unwrap().is_empty());
        let reopened = rerun(&db, owner, row.id).await.unwrap();
        assert_eq!(reopened.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(reopened.disposition, TriageDisposition::NeedsHuman);
        override_project(
            &db,
            owner,
            row.id,
            project.id,
            "corrected attribution after review",
        )
        .await
        .unwrap();
        let settled = resolve_privilege(
            &db,
            owner,
            row.id,
            PrivilegeLabel::Privileged,
            "confirmed private legal advice",
        )
        .await
        .unwrap();
        assert_eq!(settled.disposition, TriageDisposition::Overridden);
        assert!(settled.labeled_example);
        let audit = settled.rationale.unwrap();
        assert!(audit.contains("corrected attribution after review"));
        assert!(audit.contains("confirmed private legal advice"));

        });
    }

    #[tokio::test]
    async fn a_rerun_returns_a_resolved_row_to_the_queue() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Rerun Owner", Role::Owner).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Rerun Matter").await;
        participate(&db, owner, project.id).await;

        // A confirmed email row reopens as Uncertain — the portal re-runs
        // the rule layer, and email without a classifier is uncertain.
        let (triage_id, _) =
            needs_human_with_suggestion(&db, firm, "rerun-email", Some(project.id)).await;
        confirm_project(&db, owner, triage_id, project.id)
            .await
            .unwrap();
        let reopened = rerun(&db, owner, triage_id).await.unwrap();
        assert_eq!(reopened.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(reopened.disposition, TriageDisposition::NeedsHuman);
        assert_eq!(reopened.suggested_project_id, Some(project.id));
        assert!(
            list_for_review(&db, owner).await.unwrap().len() == 1,
            "the reopened row waits again"
        );

        // A labeled example survives the re-run.
        let overridden = override_project(&db, owner, triage_id, project.id, "still right")
            .await
            .unwrap();
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

        });
    }

    #[tokio::test]
    async fn an_admin_cannot_decide_another_firms_triage() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let own_firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let other_firm = crate::test_support::seed_firm_with_new_brand(&db, "vesta").await.0;
        let admin = person(&db, "Scoped Admin", Role::Admin).await;
        member(&db, admin, own_firm, FirmMembership::Admin).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(other_firm), "Other Firm Project")
                .await;
        for action in ["confirm", "override", "resolve", "rerun"] {
            let (row, _) =
                needs_human_with_suggestion(&db, other_firm, action, Some(project.id)).await;
            let result = match action {
                "confirm" => confirm_project(&db, admin, row, project.id).await,
                "override" => {
                    override_project(&db, admin, row, project.id, "synthetic correction").await
                }
                "resolve" => {
                    resolve_privilege(
                        &db,
                        admin,
                        row,
                        PrivilegeLabel::Privileged,
                        "synthetic review",
                    )
                    .await
                }
                _ => rerun(&db, admin, row).await,
            };
            assert!(
                matches!(result, Err(crate::inbox::InboxError::Denied)),
                "{action} must retain Firm scope"
            );
            assert_eq!(
                find_by_id(&db, row).await.unwrap().unwrap().disposition,
                TriageDisposition::NeedsHuman
            );
        }

        });
    }

    #[tokio::test]
    async fn automated_private_social_claims_cannot_bypass_the_store_rules() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let message = inbound(&db, firm, "x", "private-store-claim").await;
        sqlx::query("UPDATE message SET visibility = 'private' WHERE id = $1")
            .bind(message.message_id)
            .execute(pg.pool())
            .await
            .unwrap();
        let mut input = new_triage(
            message.message_id,
            firm,
            None,
            PrivilegeLabel::Privileged,
            TriageDisposition::Confirmed,
        );
        input.model_id = Some("synthetic-model".into());
        input.raw_output = Some("synthetic classifier artifact".into());
        let row = record(&db, &input).await.unwrap();
        assert_eq!(row.privilege, PrivilegeLabel::Uncertain);
        assert_eq!(row.disposition, TriageDisposition::NeedsHuman);
        assert!(row.model_id.is_none());
        assert!(row.raw_output.is_none());

        });
    }

    #[tokio::test]
    async fn a_triage_record_cannot_claim_another_messages_firm() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let other = crate::test_support::seed_firm_with_new_brand(&db, "summons").await.0;
        let message = inbound(&db, firm, "email", "foreign-record-claim").await;
        let result = record(
            &db,
            &new_triage(
                message.message_id,
                other,
                None,
                PrivilegeLabel::Uncertain,
                TriageDisposition::NeedsHuman,
            ),
        )
        .await;
        assert!(matches!(result, Err(InboxError::CrossFirm)));
        assert!(find_for_message(&db, message.message_id)
            .await
            .unwrap()
            .is_none());

        });
    }

    #[tokio::test]
    async fn target_participation_does_not_authorize_taking_another_matter_conversation() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Assignment Owner", Role::Owner).await;
        let lawyer = person(&db, "Target Reviewer", Role::Lawyer).await;
        let source =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Unseen Source Matter")
                .await;
        let target =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Visible Target Matter")
                .await;
        participate(&db, lawyer, target.id).await;
        let (row, message) =
            needs_human_with_suggestion(&db, firm, "unseen-reassignment", Some(target.id)).await;
        crate::inbox::assign(&db, owner, message.conversation_id, source.id)
            .await
            .unwrap();
        let original = find_by_id(&db, row).await.unwrap().unwrap();
        let denied = override_project(&db, lawyer, row, target.id, "move into my matter").await;
        assert!(
            matches!(denied, Err(InboxError::Denied)),
            "target participation cannot grant access to source content"
        );
        assert_eq!(find_by_id(&db, row).await.unwrap().unwrap(), original);
        assert_eq!(
            conversation_for_message(&db, message.message_id)
                .await
                .unwrap()
                .project_id,
            Some(source.id)
        );
        assert_eq!(
            crate::inbox::assignments(&db, message.conversation_id)
                .await
                .unwrap()
                .len(),
            1
        );
        participate(&db, lawyer, source.id).await;
        override_project(&db, lawyer, row, target.id, "reviewed both matters")
            .await
            .unwrap();
        assert_eq!(
            conversation_for_message(&db, message.message_id)
                .await
                .unwrap()
                .project_id,
            Some(target.id)
        );

        });
    }

    #[tokio::test]
    async fn firm_membership_does_not_authorize_review_of_an_unseen_assigned_matter() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let owner = person(&db, "Scope Owner", Role::Owner).await;
        let lawyer = person(&db, "synthetic reviewer", Role::Lawyer).await;
        member(&db, lawyer, firm, FirmMembership::Lawyer).await;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Restricted Review Matter")
                .await;
        let (row, message) =
            needs_human_with_suggestion(&db, firm, "unseen-review", Some(project.id)).await;
        crate::inbox::assign(&db, owner, message.conversation_id, project.id)
            .await
            .unwrap();
        let original = find_by_id(&db, row).await.unwrap().unwrap();
        let denied = resolve_privilege(
            &db,
            lawyer,
            row,
            PrivilegeLabel::Privileged,
            "not my matter",
        )
        .await;
        assert!(matches!(denied, Err(InboxError::Denied)));
        assert_eq!(find_by_id(&db, row).await.unwrap().unwrap(), original);
        let denied = rerun(&db, lawyer, row).await;
        assert!(matches!(denied, Err(InboxError::Denied)));
        assert_eq!(find_by_id(&db, row).await.unwrap().unwrap(), original);
        participate(&db, lawyer, project.id).await;
        resolve_privilege(
            &db,
            lawyer,
            row,
            PrivilegeLabel::Privileged,
            "reviewed my matter",
        )
        .await
        .unwrap();
        assert_eq!(
            rerun(&db, lawyer, row).await.unwrap().privilege,
            PrivilegeLabel::Uncertain
        );

        });
    }

    #[tokio::test]
    async fn assigned_matter_audit_content_requires_project_participation() {
        #[rustfmt::skip]
        with_native_triage_fixture!(db, pg, {
let firm = crate::test_support::seed_firm_with_new_brand(&db, "neon").await.0;
        let project =
            crate::test_support::seed_project_on_firm(&db, Some(firm), "Private Audit Project")
                .await;
        let owner = person(&db, "Audit Owner", Role::Owner).await;
        let lawyer = person(&db, "Audit Lawyer", Role::Lawyer).await;
        member(&db, lawyer, firm, FirmMembership::Lawyer).await;
        let message = inbound(&db, firm, "email", "scoped-audit").await;
        crate::inbox::assign(&db, owner, message.conversation_id, project.id)
            .await
            .unwrap();
        let mut input = new_triage(
            message.message_id,
            firm,
            Some(project.id),
            PrivilegeLabel::Uncertain,
            TriageDisposition::NeedsHuman,
        );
        input.rationale = Some("Synthetic private message detail".into());
        input.raw_output = Some("Synthetic private classifier output".into());
        record(&db, &input).await.unwrap();
        let hidden = list_for_review(&db, lawyer).await.unwrap();
        assert_eq!(hidden.len(), 1);
        assert!(
            hidden[0].triage.rationale.is_none(),
            "Firm membership does not expose matter content"
        );
        assert!(hidden[0].triage.raw_output.is_none());
        participate(&db, lawyer, project.id).await;
        let visible = list_for_review(&db, lawyer).await.unwrap();
        assert_eq!(visible[0].triage.rationale, input.rationale);
        assert_eq!(visible[0].triage.raw_output, input.raw_output);

        });
    }
}
