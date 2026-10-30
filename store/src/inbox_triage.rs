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
}
