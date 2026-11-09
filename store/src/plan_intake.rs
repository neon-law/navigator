//! The prepare-and-confirm plan intake: a parsed plan assignment is
//! staged for a person to confirm, and a matter exists only on the
//! attorney's attested acceptance.
//!
//! Staging ([`stage`]) is the automated half, run from the portal's
//! plan-assignment lane the moment a notice parses. It anchors the member
//! (a `Role::Client` person and a fresh `Human` entity, the same shape the
//! self-serve retainer walk creates), resolves the firm's matching
//! [`crate::legal_plans::LegalPlan`] and its fee quote, runs
//! [`crate::conflicts::check_new_matter`] pre-write, and records a
//! [`StagedAssignment`]. It never opens a matter — no code path in this
//! module opens one, and the record's status refuses acceptance that did
//! not come through [`accept`] with the attorney's own attestation, which
//! [`crate::projects::open_matter`] enforces again at the open.
//!
//! Acceptance ([`accept`]) is the person's half, driven by the review
//! screen's confirm door: open the matter (attestation forwarded from the
//! click, never defaulted), fill the project's plan columns with the
//! assignment's date as `plan_assigned_on`, create the plan's SLA
//! deadlines from the email-landing time, and mark the record accepted.
//! The member intro and plan reply are the portal's sends; both key off
//! the staged record so a replay cannot double-send.
//!
//! The staging row is SurrealDB-authored, like a lead or a triage row: it
//! is transient pre-matter intake state, not a postgres-mirrored domain.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use surrealdb::types::{RecordId, SurrealValue};
use uuid::Uuid;

use crate::legal_plans::{LegalPlan, PlanMatterType};
use crate::postgres::PostgresDb;
use crate::surreal::{record_id, record_uuid, SurrealDb};

const TABLE: &str = "plan_intake";
const SELECT: &str = "id, firm_id, conversation_id, source_message_id, provider, member_name, \
     member_email, member_person_id, member_entity_id, plan_member_id, benefit_code, plan_number, \
     plan_matter_type, plan_state, assigned_at, legal_plan_id, legal_plan_name, plan_portal_only, \
     fee_covered_benefit, fee_plan_fee_cents, fee_supplemental_flat_cents, \
     fee_supplemental_hourly_cents, fee_effective_on, conflict_summary, conflict_blocking, \
     conflict_checked, status, project_id, accepted_by, decided_at, decline_reason, \
     inserted_at, updated_at";

/// A parsed plan assignment held for a person's confirmation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StagedAssignment {
    pub id: Uuid,
    pub firm_id: Uuid,
    /// The assignment email's conversation — where the member intro thread
    /// and the reserved outbound live.
    pub conversation_id: Uuid,
    /// The ingested inbound Message the notice produced. Staging is
    /// idempotent on this id, and the member intro's reserve-then-send
    /// anchor reconstructs from it.
    pub source_message_id: Uuid,
    /// The provider spelling from [`crate::plan_assignment::PlanProvider::as_str`].
    pub provider: String,
    pub member_name: String,
    pub member_email: Option<String>,
    /// The member's anchored client person, when the notice printed a
    /// contact email. `None` leaves the conflict check unrun and refuses
    /// acceptance until a person can anchor the matter.
    pub member_person_id: Option<Uuid>,
    /// A fresh `Human` entity for the member, created beside the person.
    pub member_entity_id: Option<Uuid>,
    /// The notice's member number — the project's `plan_member_id`.
    pub plan_member_id: Option<String>,
    /// The notice's plan/certificate number, verbatim — becomes the
    /// project's `plan_case_number` at acceptance. The notice names no
    /// benefit code, so `benefit_code` stages empty.
    pub plan_number: Option<String>,
    /// A benefit code the plan's assignment named, when it ever does.
    pub benefit_code: Option<String>,
    /// The classified matter type, as [`PlanMatterType::as_str`] spells it.
    pub plan_matter_type: String,
    /// The lowercased state word from the notice when it survives
    /// [`crate::legal_plans::valid_state_code`].
    pub plan_state: Option<String>,
    /// When the assignment email landed. The acceptance clock starts here,
    /// not when a person reads the notice.
    pub assigned_at: DateTime<Utc>,
    /// The firm's matching plan, resolved from PostgreSQL at staging.
    /// `None` when no plan matches (or PostgreSQL is not configured); the
    /// review screen asks the attorney to pick one before acceptance.
    pub legal_plan_id: Option<Uuid>,
    pub legal_plan_name: Option<String>,
    /// Whether the plan's acceptance is confirmed on the provider's portal
    /// (`LegalPlan.portal` set) instead of by reply email.
    pub plan_portal_only: bool,
    /// The fee quote snapshot, resolved from the plan's schedule. Kept on
    /// the record so the screen shows what the schedule said when the
    /// notice landed, and the accepted matter keeps the pricing identity
    /// the margin queries resolve from the plan columns.
    pub fee_covered_benefit: Option<String>,
    pub fee_plan_fee_cents: Option<i32>,
    pub fee_supplemental_flat_cents: Option<i32>,
    pub fee_supplemental_hourly_cents: Option<i32>,
    pub fee_effective_on: Option<NaiveDate>,
    /// One line per conflict finding, from [`crate::conflicts`].
    pub conflict_summary: Vec<String>,
    /// At least one blocking finding — open_matter will refuse.
    pub conflict_blocking: bool,
    /// Whether the conflict check ran at all (it cannot without an
    /// anchored member).
    pub conflict_checked: bool,
    /// A [`Status`] spelling.
    pub status: String,
    /// The opened matter, set by [`accept`].
    pub project_id: Option<Uuid>,
    pub accepted_by: Option<Uuid>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Closed status set stored on `plan_intake.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Staged,
    Accepted,
    Declined,
}

impl Status {
    /// Stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
        }
    }
}

/// Everything the lane hands staging: the parsed assignment plus the
/// inbound coordinates it landed under.
#[derive(Debug, Clone)]
pub struct StageInput {
    pub firm_id: Uuid,
    pub conversation_id: Uuid,
    pub source_message_id: Uuid,
    /// When the assignment email landed (the ingested Message's
    /// `inserted_at`).
    pub landing_at: DateTime<Utc>,
    /// [`crate::plan_assignment::PlanProvider::as_str`] of the resolved sender.
    pub provider: String,
    /// The parsed notice.
    pub assignment: crate::plan_assignment::Assignment,
}

/// The attorney's acceptance of one staged assignment. `attestation` is
/// forwarded from the review screen's explicit confirmation and is never
/// defaulted — [`crate::projects::open_matter`] refuses a missing one, so
/// no caller can open the matter without the click.
#[derive(Debug, Clone)]
pub struct AcceptInput {
    pub attestation: bool,
    /// The accepting attorney — the accountable lawyer DRI on the audit row.
    pub acting_person_id: Uuid,
    /// The brand the matter opens through, resolved server-side by the door.
    pub brand: String,
    /// The matter code, chosen once. The review screen prefills
    /// [`suggest_code`]; a collision is refused, never silently resolved.
    pub code: String,
    /// The matter's display name; the screen prefills [`default_matter_name`].
    pub name: Option<String>,
    /// The plan to work under — the firm's own plan, verified here.
    pub legal_plan_id: Uuid,
    /// The matter type to work the matter as, correctable on the screen.
    pub plan_matter_type: String,
    /// The jurisdiction to price and work the matter under, correctable.
    pub plan_state: Option<String>,
    /// The firm's client-facing intake questions, carried in the member
    /// intro ([`member_intro_body`]).
    pub questions: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanIntakeError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("writing a staged assignment returned no usable row")]
    WriteReturnedNothing,
    #[error("unknown staged assignment")]
    NotFound,
    #[error("the assignment is no longer staged")]
    NotStaged,
    #[error("the notice has no member contact email, so no member person anchors the matter")]
    MissingMemberAnchor,
    #[error("plan does not belong to the firm, or does not exist")]
    PlanNotFound,
    #[error("matter_type must be one of the plan vocabulary's words")]
    InvalidMatterType,
    #[error("state must be a lowercase jurisdiction code the fee schedule holds")]
    InvalidState,
    #[error("conflict check failed: {0}")]
    Conflict(String),
    #[error(transparent)]
    Open(#[from] crate::projects::OpenMatterError),
    #[error(transparent)]
    ProjectCommand(#[from] crate::projects::ProjectCommandError),
    #[error(transparent)]
    ProjectStore(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Deadline(#[from] crate::project_deadlines::DeadlineError),
    #[error(transparent)]
    Person(#[from] crate::persons::PersonError),
    #[error(transparent)]
    Entity(#[from] crate::entities::EntityError),
    #[error(transparent)]
    EntityType(#[from] crate::entity_types::EntityTypeError),
    #[error(transparent)]
    Jurisdiction(#[from] crate::jurisdictions::JurisdictionError),
    #[error(transparent)]
    LegalPlan(#[from] crate::legal_plans::LegalPlanError),
}

#[derive(SurrealValue)]
struct StagedRow {
    id: RecordId,
    firm_id: RecordId,
    conversation_id: RecordId,
    source_message_id: RecordId,
    provider: String,
    member_name: String,
    member_email: Option<String>,
    member_person_id: Option<RecordId>,
    member_entity_id: Option<RecordId>,
    plan_member_id: Option<String>,
    benefit_code: Option<String>,
    plan_number: Option<String>,
    plan_matter_type: String,
    plan_state: Option<String>,
    assigned_at: surrealdb::types::Datetime,
    legal_plan_id: Option<String>,
    legal_plan_name: Option<String>,
    plan_portal_only: bool,
    fee_covered_benefit: Option<String>,
    fee_plan_fee_cents: Option<i32>,
    fee_supplemental_flat_cents: Option<i32>,
    fee_supplemental_hourly_cents: Option<i32>,
    fee_effective_on: Option<String>,
    conflict_summary: Option<String>,
    conflict_blocking: bool,
    conflict_checked: bool,
    status: String,
    project_id: Option<RecordId>,
    accepted_by: Option<RecordId>,
    decided_at: Option<surrealdb::types::Datetime>,
    decline_reason: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl StagedRow {
    fn into_staged(self) -> Option<StagedAssignment> {
        Some(StagedAssignment {
            id: record_uuid(&self.id)?,
            firm_id: record_uuid(&self.firm_id)?,
            conversation_id: record_uuid(&self.conversation_id)?,
            source_message_id: record_uuid(&self.source_message_id)?,
            provider: self.provider,
            member_name: self.member_name,
            member_email: self.member_email,
            member_person_id: self.member_person_id.as_ref().and_then(record_uuid),
            member_entity_id: self.member_entity_id.as_ref().and_then(record_uuid),
            plan_member_id: self.plan_member_id,
            benefit_code: self.benefit_code,
            plan_number: self.plan_number,
            plan_matter_type: self.plan_matter_type,
            plan_state: self.plan_state,
            assigned_at: self.assigned_at.into(),
            // A legal_plan row lives in PostgreSQL; the source keeps the
            // bare UUID, like the project's plan columns do.
            legal_plan_id: self
                .legal_plan_id
                .and_then(|raw| Uuid::parse_str(&raw).ok()),
            legal_plan_name: self.legal_plan_name,
            plan_portal_only: self.plan_portal_only,
            fee_covered_benefit: self.fee_covered_benefit,
            fee_plan_fee_cents: self.fee_plan_fee_cents,
            fee_supplemental_flat_cents: self.fee_supplemental_flat_cents,
            fee_supplemental_hourly_cents: self.fee_supplemental_hourly_cents,
            fee_effective_on: self
                .fee_effective_on
                .and_then(|raw| NaiveDate::parse_from_str(&raw, "%Y-%m-%d").ok()),
            conflict_summary: self
                .conflict_summary
                .unwrap_or_default()
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
            conflict_blocking: self.conflict_blocking,
            conflict_checked: self.conflict_checked,
            status: self.status,
            project_id: self.project_id.as_ref().and_then(record_uuid),
            accepted_by: self.accepted_by.as_ref().and_then(record_uuid),
            decided_at: self.decided_at.map(Into::into),
            decline_reason: self.decline_reason,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// Classify a notice's matter-type words into the closed plan vocabulary.
/// The review screen shows the classification beside the notice's own
/// words so the attorney can correct it before acceptance.
#[must_use]
pub fn matter_type_from_notice(text: &str) -> PlanMatterType {
    let lowered = text.to_lowercase();
    if lowered.contains("divorce") {
        PlanMatterType::UncontestedDivorce
    } else if lowered.contains("letter") {
        PlanMatterType::Letter
    } else if lowered.contains("appearance") || lowered.contains("hearing") {
        PlanMatterType::CourtAppearance
    } else {
        PlanMatterType::DocumentReview
    }
}

/// The matter type's display label, for screen copy and matter names.
#[must_use]
pub fn matter_label(matter_type: PlanMatterType) -> &'static str {
    match matter_type {
        PlanMatterType::DocumentReview => "document review",
        PlanMatterType::Letter => "letter",
        PlanMatterType::UncontestedDivorce => "uncontested divorce",
        PlanMatterType::CourtAppearance => "court appearance",
    }
}

/// The firm's plan that matches a parsed provider, by name — a firm's
/// ARAG row is theirs to name, so the match is a lowercase containment,
/// not an equality. `provider` is [`crate::plan_assignment::PlanProvider::as_str`].
#[must_use]
pub fn plan_for_provider<'a>(plans: &'a [LegalPlan], provider: &str) -> Option<&'a LegalPlan> {
    plans.iter().find(|plan| {
        let name = plan.name.to_lowercase();
        match provider {
            "arag" => name.contains("arag"),
            "legal_shield" => name.contains("legalshield") || name.contains("legal shield"),
            _ => false,
        }
    })
}

/// The acceptance deadline's due instant: the plan's acceptance SLA
/// measured from the moment the assignment email landed — never from
/// when a person happened to read or accept it.
#[must_use]
pub fn acceptance_due(landing: DateTime<Utc>, acceptance_sla_hours: i32) -> DateTime<Utc> {
    landing + Duration::hours(i64::from(acceptance_sla_hours))
}

/// The turnaround deadline's due instant: the plan's turnaround SLA
/// measured from acceptance.
#[must_use]
pub fn turnaround_due(accepted_at: DateTime<Utc>, turnaround_sla_days: i32) -> DateTime<Utc> {
    accepted_at + Duration::days(i64::from(turnaround_sla_days))
}

/// The matter-code prefill for a staged assignment: the plan member id
/// lowercased and reduced to the code vocabulary's letters, digits, and
/// hyphens, under a `plan-` prefix. The attorney owns the final code; a
/// collision with an open matter is refused, never silently resolved.
#[must_use]
pub fn suggest_code(plan_member_id: &str) -> String {
    let reduced: String = plan_member_id
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut code = format!("plan-{reduced}");
    while code.ends_with('-') {
        code.pop();
    }
    code
}

/// The matter-name prefill for a staged assignment.
#[must_use]
pub fn default_matter_name(
    plan_name: &str,
    member_name: &str,
    matter_type: PlanMatterType,
) -> String {
    format!(
        "{plan_name} plan matter — {} ({})",
        member_name,
        matter_label(matter_type)
    )
}

/// The member's intro message: what the firm sends the member on
/// acceptance. The SMS-consent question is always asked — consent is
/// never assumed from the plan having a phone number on file. The
/// firm's client-facing questions ride along when the firm has any;
/// otherwise the intro promises the questionnaire separately rather than
/// inventing one.
#[must_use]
pub fn member_intro_body(
    member_name: &str,
    firm_name: &str,
    matter_label: &str,
    questions: &[String],
) -> String {
    let mut body = format!(
        "Hello {member_name},\n\n\
         Your legal plan assigned your {matter_label} matter to {firm_name}, and we have \
         accepted it. An attorney will reach out to schedule your consultation.\n\n"
    );
    if questions.is_empty() {
        body.push_str(
            "We will send a short intake questionnaire for your matter — no need to \
             prepare anything before then.\n\n",
        );
    } else {
        body.push_str("To get started, a few questions you can answer in a reply:\n\n");
        for question in questions {
            body.push_str("- ");
            body.push_str(question);
            body.push('\n');
        }
        body.push('\n');
    }
    body.push_str(
        "One question we have to ask: may we text you at the number your plan has \
         on file about scheduling? Replying YES or NO is entirely optional, and we \
         will not text without your answer.\n\n",
    );
    body.push_str("— ");
    body.push_str(firm_name);
    body.push('\n');
    body
}

/// Everything staging binds from the firm's reference data: the member
/// anchor, the firm's plan for the provider with its fee quote, and the
/// pre-write conflict report. None of it writes to a matter.
struct StagingContext {
    member_person_id: Option<Uuid>,
    member_entity_id: Option<Uuid>,
    legal_plan_id: Option<Uuid>,
    legal_plan_name: Option<String>,
    plan_portal_only: bool,
    fee: Option<ResolvedFee>,
    conflict_summary: Vec<String>,
    conflict_blocking: bool,
    conflict_checked: bool,
}

async fn staging_context(
    surreal: &SurrealDb,
    pg: Option<&PostgresDb>,
    input: &StageInput,
    matter_type: PlanMatterType,
) -> Result<StagingContext, PlanIntakeError> {
    let assignment = &input.assignment;
    let member = anchor_member(surreal, assignment).await?;
    let (member_person_id, member_entity_id) = match member {
        Some((person_id, entity_id)) => (Some(person_id), Some(entity_id)),
        None => (None, None),
    };

    // The firm's plan for this provider, and the fee quote the schedule
    // resolves for the classified matter type and the notice's state.
    let (legal_plan_id, legal_plan_name, plan_portal_only, fee) = resolve_plan_and_fee(
        pg,
        input.firm_id,
        &input.provider,
        matter_type,
        assignment.state.as_deref(),
        assignment
            .effective_date
            .unwrap_or_else(|| input.landing_at.date_naive()),
    )
    .await?;

    // The pre-write conflict check, anchored on the member. open_matter
    // runs it again at acceptance; the staged report is what the review
    // screen shows so the attorney sees conflicts before clicking.
    let (conflict_summary, conflict_blocking, conflict_checked) =
        conflict_report(surreal, member).await?;

    Ok(StagingContext {
        member_person_id,
        member_entity_id,
        legal_plan_id,
        legal_plan_name,
        plan_portal_only,
        fee,
        conflict_summary,
        conflict_blocking,
        conflict_checked,
    })
}

/// The member anchor: a `Role::Client` person for the notice's contact
/// email, and a fresh `Human` entity beside it — the same shape the
/// self-serve retainer walk creates for a solo natural person. A notice
/// without a contact email stages unanchored; the review screen resolves
/// the member before acceptance.
async fn anchor_member(
    surreal: &SurrealDb,
    assignment: &crate::plan_assignment::Assignment,
) -> Result<Option<(Uuid, Uuid)>, PlanIntakeError> {
    let Some(email) = &assignment.contact_email else {
        return Ok(None);
    };
    let person = match crate::persons::find_by_email_ci(surreal, email).await? {
        Some(person) => person,
        None => {
            crate::persons::create(
                surreal,
                &crate::persons::NewPerson {
                    name: assignment.member_name.clone(),
                    email: email.clone(),
                    role: crate::persons::Role::Client,
                    ..crate::persons::NewPerson::default()
                },
            )
            .await?
        }
    };
    let entity = create_human_entity(surreal, &assignment.member_name).await?;
    Ok(Some((person.id, entity)))
}

/// The pre-write conflict report for the member anchor. Unanchored notices
/// stage with an explanatory note instead of a check.
async fn conflict_report(
    surreal: &SurrealDb,
    member: Option<(Uuid, Uuid)>,
) -> Result<(Vec<String>, bool, bool), PlanIntakeError> {
    match member {
        Some((person_id, entity_id)) => {
            let report = crate::conflicts::check_new_matter(surreal, person_id, entity_id)
                .await
                .map_err(PlanIntakeError::Conflict)?;
            Ok((report.summary_lines(), report.has_blocking(), true))
        }
        None => Ok((
            vec![
                "The notice has no member contact email, so no member person \
                 anchors the matter and the conflict check has not run."
                    .to_string(),
            ],
            false,
            false,
        )),
    }
}

/// Stage one parsed assignment for a person's confirmation. Idempotent per
/// `StageInput::source_message_id`: a redelivered notice lands on the
/// already-staged record instead of a second row. Never opens a matter —
/// a matter exists only through [`accept`].
///
/// `pg` is the firm's PostgreSQL handle when configured; without it the
/// plan and fee columns stage empty and the review screen asks the
/// attorney to pick the plan before acceptance.
///
/// # Errors
///
/// [`PlanIntakeError`] for database or reference-data failures, or a
/// failing conflict check.
pub async fn stage(
    surreal: &SurrealDb,
    pg: Option<&PostgresDb>,
    input: &StageInput,
) -> Result<StagedAssignment, PlanIntakeError> {
    if let Some(existing) = find_by_source_message(surreal, input.source_message_id).await? {
        return Ok(existing);
    }

    let assignment = &input.assignment;
    let matter_type = matter_type_from_notice(&assignment.matter_type);
    let context = staging_context(surreal, pg, input, matter_type).await?;
    let now = Utc::now();
    let id = Uuid::now_v7();
    let mut response = surreal
        .query(format!(
            "CREATE $id SET firm_id = $firm_id, conversation_id = $conversation_id, \
             source_message_id = $source_message_id, provider = $provider, \
             member_name = $member_name, member_email = $member_email, \
             member_person_id = $member_person_id, member_entity_id = $member_entity_id, \
             plan_member_id = $plan_member_id, plan_number = $plan_number, \
             plan_matter_type = $plan_matter_type, plan_state = $plan_state, \
             assigned_at = $assigned_at, legal_plan_id = $legal_plan_id, \
             legal_plan_name = $legal_plan_name, plan_portal_only = $plan_portal_only, \
             fee_covered_benefit = $fee_covered_benefit, \
             fee_plan_fee_cents = $fee_plan_fee_cents, \
             fee_supplemental_flat_cents = $fee_supplemental_flat_cents, \
             fee_supplemental_hourly_cents = $fee_supplemental_hourly_cents, \
             fee_effective_on = $fee_effective_on, conflict_summary = $conflict_summary, \
             conflict_blocking = $conflict_blocking, conflict_checked = $conflict_checked, \
             status = 'staged', project_id = NONE, accepted_by = NONE, decided_at = NONE, \
             decline_reason = NONE, inserted_at = $now, updated_at = $now RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("firm_id", record_id("firm", input.firm_id)))
        .bind((
            "conversation_id",
            record_id("inbox_conversation", input.conversation_id),
        ))
        .bind((
            "source_message_id",
            record_id("message", input.source_message_id),
        ))
        .bind(("provider", input.provider.clone()))
        .bind(("member_name", assignment.member_name.clone()))
        .bind(("member_email", assignment.contact_email.clone()))
        .bind((
            "member_person_id",
            context.member_person_id.map(|id| record_id("person", id)),
        ))
        .bind((
            "member_entity_id",
            context.member_entity_id.map(|id| record_id("entity", id)),
        ))
        .bind(("plan_member_id", Some(assignment.member_number.clone())))
        // The notice names no benefit code, so `benefit_code` stages
        // empty; the verbatim plan/certificate number is the project's
        // future `plan_case_number`.
        .bind(("plan_number", assignment.plan_number.clone()))
        .bind(("plan_matter_type", matter_type.as_str()))
        .bind(("plan_state", sanitized_state(assignment.state.as_deref())))
        .bind((
            "assigned_at",
            surrealdb::types::Datetime::from(input.landing_at),
        ))
        .bind((
            "legal_plan_id",
            context.legal_plan_id.map(|id| id.to_string()),
        ))
        .bind(("legal_plan_name", context.legal_plan_name))
        .bind(("plan_portal_only", context.plan_portal_only))
        .bind((
            "fee_covered_benefit",
            context.fee.as_ref().map(|fee| fee.0.clone()),
        ))
        .bind(("fee_plan_fee_cents", context.fee.as_ref().map(|fee| fee.1)))
        .bind((
            "fee_supplemental_flat_cents",
            context.fee.as_ref().and_then(|fee| fee.2),
        ))
        .bind((
            "fee_supplemental_hourly_cents",
            context.fee.as_ref().and_then(|fee| fee.3),
        ))
        .bind((
            "fee_effective_on",
            context.fee.as_ref().map(|fee| fee.4.to_string()),
        ))
        .bind(("conflict_summary", context.conflict_summary.join("\n")))
        .bind(("conflict_blocking", context.conflict_blocking))
        .bind(("conflict_checked", context.conflict_checked))
        .bind(("now", surrealdb::types::Datetime::from(now)))
        .await?
        .check()?;

    let staged = response
        .take::<Option<StagedRow>>(0)?
        .and_then(StagedRow::into_staged)
        .ok_or(PlanIntakeError::WriteReturnedNothing)?;
    Ok(staged)
}

/// The accepted state of one staged assignment: the opened matter's id
/// and the record carrying the plan columns as filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub staged: StagedAssignment,
    pub project_id: Uuid,
}

async fn staged_for_acceptance(
    surreal: &SurrealDb,
    staged_id: Uuid,
) -> Result<StagedAssignment, PlanIntakeError> {
    let staged = find_by_id(surreal, staged_id)
        .await?
        .ok_or(PlanIntakeError::NotFound)?;
    if staged.status != Status::Staged.as_str() {
        return Err(PlanIntakeError::NotStaged);
    }
    Ok(staged)
}

fn validated_matter_type(input: &AcceptInput) -> Result<PlanMatterType, PlanIntakeError> {
    let matter_type = PlanMatterType::parse(&input.plan_matter_type)
        .map_err(|_| PlanIntakeError::InvalidMatterType)?;
    if let Some(state) = &input.plan_state {
        if !crate::legal_plans::valid_state_code(state) {
            return Err(PlanIntakeError::InvalidState);
        }
    }
    Ok(matter_type)
}

/// The matter name, description, and the open itself. Retry-safe: a
/// previous accept that opened the matter and died before marking the
/// record resumes onto the same project by its chosen code instead of
/// colliding.
async fn open_on_acceptance(
    surreal: &SurrealDb,
    staged: &StagedAssignment,
    plan: &crate::legal_plans::LegalPlan,
    person_id: Uuid,
    entity_id: Uuid,
    input: &AcceptInput,
    matter_type: PlanMatterType,
) -> Result<crate::projects::Project, PlanIntakeError> {
    let name = input
        .name
        .clone()
        .unwrap_or_else(|| default_matter_name(&plan.name, &staged.member_name, matter_type));
    let description = format!(
        "Accepted from a {} assignment notice for member {} ({}).",
        staged.provider,
        staged.plan_member_id.as_deref().unwrap_or("unknown"),
        staged.member_name
    );
    let existing = crate::projects::find_by_code(surreal, &input.code).await?;
    let project = match existing {
        Some(project) if project.firm_id == Some(staged.firm_id) => project,
        Some(_) => return Err(PlanIntakeError::PlanNotFound),
        None => {
            crate::projects::open_matter(
                surreal,
                &crate::projects::OpenMatterCommand {
                    firm_id: Some(staged.firm_id),
                    name,
                    code: input.code.clone(),
                    client_id: person_id,
                    entity_id,
                    jurisdiction_id: None,
                    description: Some(description),
                    brand: input.brand.clone(),
                    attestation: input.attestation,
                    acting_person_id: input.acting_person_id,
                    closed_at: None,
                },
            )
            .await?
        }
    };
    Ok(project)
}

/// Mark the record accepted with the plan columns as filed and return the
/// updated row.
async fn mark_accepted(
    surreal: &SurrealDb,
    staged: &StagedAssignment,
    project_id: Uuid,
    input: &AcceptInput,
    matter_type: PlanMatterType,
    plan: &crate::legal_plans::LegalPlan,
    fee: Option<crate::legal_plans::PlanFeeSchedule>,
) -> Result<StagedAssignment, PlanIntakeError> {
    let now = Utc::now();
    let mut response = surreal
        .query(format!(
            "UPDATE $id SET status = 'accepted', project_id = $project_id, \
             accepted_by = $accepted_by, decided_at = $decided_at, \
             plan_matter_type = $plan_matter_type, plan_state = $plan_state, \
             legal_plan_id = $legal_plan_id, legal_plan_name = $legal_plan_name, \
             plan_portal_only = $plan_portal_only, fee_covered_benefit = $fee_covered_benefit, \
             fee_plan_fee_cents = $fee_plan_fee_cents, \
             fee_supplemental_flat_cents = $fee_supplemental_flat_cents, \
             fee_supplemental_hourly_cents = $fee_supplemental_hourly_cents, \
             fee_effective_on = $fee_effective_on, updated_at = $updated_at RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, staged.id)))
        .bind(("project_id", record_id("project", project_id)))
        .bind(("accepted_by", record_id("person", input.acting_person_id)))
        .bind(("decided_at", surrealdb::types::Datetime::from(now)))
        .bind(("plan_matter_type", matter_type.as_str()))
        .bind(("plan_state", input.plan_state.clone()))
        .bind(("legal_plan_id", Some(plan.id.to_string())))
        .bind(("legal_plan_name", Some(plan.name.clone())))
        .bind(("plan_portal_only", plan.portal.is_some()))
        .bind((
            "fee_covered_benefit",
            fee.as_ref().map(|fee| fee.covered_benefit.clone()),
        ))
        .bind((
            "fee_plan_fee_cents",
            fee.as_ref().map(|fee| fee.plan_fee_cents),
        ))
        .bind((
            "fee_supplemental_flat_cents",
            fee.as_ref().and_then(|fee| fee.supplemental_flat_cents),
        ))
        .bind((
            "fee_supplemental_hourly_cents",
            fee.as_ref().and_then(|fee| fee.supplemental_hourly_cents),
        ))
        .bind((
            "fee_effective_on",
            fee.as_ref().map(|fee| fee.effective_on.to_string()),
        ))
        .bind(("updated_at", surrealdb::types::Datetime::from(now)))
        .await?
        .check()?;
    let staged = response
        .take::<Option<StagedRow>>(0)?
        .and_then(StagedRow::into_staged)
        .ok_or(PlanIntakeError::WriteReturnedNothing)?;
    Ok(staged)
}

/// Accept a staged assignment: open the matter on the attorney's
/// attestation, fill its plan columns (with the email-landing date as
/// `plan_assigned_on`), create the plan's SLA deadlines, and mark the
/// record accepted. The attestation is forwarded from the caller —
/// [`crate::projects::open_matter`] refuses to open without it, so no
/// path through this function opens a matter without the attorney's click.
///
/// The member intro and the plan's reply are the portal's sends, keyed
/// off the accepted record; this function never sends mail.
///
/// # Errors
///
/// [`PlanIntakeError::NotStaged`] when the record was already accepted or
/// declined; [`PlanIntakeError::MissingMemberAnchor`] when no member
/// person anchors the matter; [`PlanIntakeError::Open`] when the open
/// refused (missing attestation, blocking conflicts, a chosen code's
/// collision, a non-lawyer attester); [`PlanIntakeError::PlanNotFound`]
/// when the plan is not the firm's own.
pub async fn accept(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    staged_id: Uuid,
    input: &AcceptInput,
) -> Result<Accepted, PlanIntakeError> {
    let staged = staged_for_acceptance(surreal, staged_id).await?;
    let person_id = staged
        .member_person_id
        .ok_or(PlanIntakeError::MissingMemberAnchor)?;
    let entity_id = staged
        .member_entity_id
        .ok_or(PlanIntakeError::MissingMemberAnchor)?;
    let matter_type = validated_matter_type(input)?;

    // The plan is the firm's own — the lookup is firm-scoped, so another
    // firm's plan id refuses here.
    let plan = crate::legal_plans::find(pg, staged.firm_id, input.legal_plan_id)
        .await?
        .ok_or(PlanIntakeError::PlanNotFound)?;

    let project = open_on_acceptance(
        surreal,
        &staged,
        &plan,
        person_id,
        entity_id,
        input,
        matter_type,
    )
    .await?;
    crate::projects::set_plan_columns(
        surreal,
        pg,
        project.id,
        &crate::projects::PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_member_id: staged.plan_member_id.clone(),
            plan_case_number: staged.plan_number.clone(),
            benefit_code: staged.benefit_code.clone(),
            plan_matter_type: Some(input.plan_matter_type.clone()),
            plan_state: input.plan_state.clone(),
            // The assignment date is the email-landing date, not the
            // acceptance date — the plan's clock started at landing.
            plan_assigned_on: Some(staged.assigned_at.date_naive().to_string()),
            plan_accepted_on: Some(Utc::now().date_naive().to_string()),
            plan_closed_on: None,
            // The kit catalog is compiled from the project-skill kits;
            // until ENG-1271's catalog matches plan matter types, no kit
            // version applies and the column stays empty.
            kit_version: None,
        },
    )
    .await?;

    // The quote as accepted, from the corrected matter type and state.
    let fee = if let Some(state) = &input.plan_state {
        crate::legal_plans::resolve_fee(
            pg,
            staged.firm_id,
            plan.id,
            matter_type,
            state,
            staged
                .fee_effective_on
                .unwrap_or_else(|| Utc::now().date_naive()),
        )
        .await?
    } else {
        None
    };

    // The plan's SLA deadlines, from the email-landing time.
    create_plan_deadlines(surreal, project.id, &staged, &plan, &input.code).await?;

    let staged =
        mark_accepted(surreal, &staged, project.id, input, matter_type, &plan, fee).await?;
    Ok(Accepted {
        staged,
        project_id: project.id,
    })
}

/// Decline a staged assignment without opening a matter. The record and
/// its conflict report stay auditable; the member's person and entity rows
/// remain — they are directory data, not matter data.
///
/// # Errors
///
/// [`PlanIntakeError::NotStaged`] when the record was already decided.
pub async fn decline(
    surreal: &SurrealDb,
    staged_id: Uuid,
    reason: &str,
) -> Result<StagedAssignment, PlanIntakeError> {
    let staged = find_by_id(surreal, staged_id)
        .await?
        .ok_or(PlanIntakeError::NotFound)?;
    if staged.status != Status::Staged.as_str() {
        return Err(PlanIntakeError::NotStaged);
    }
    let now = Utc::now();
    let mut response = surreal
        .query(format!(
            "UPDATE $id SET status = 'declined', decided_at = $decided_at, \
             decline_reason = $reason, updated_at = $updated_at RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, staged.id)))
        .bind(("decided_at", surrealdb::types::Datetime::from(now)))
        .bind(("reason", reason.trim().to_string()))
        .bind(("updated_at", surrealdb::types::Datetime::from(now)))
        .await?
        .check()?;

    response
        .take::<Option<StagedRow>>(0)?
        .and_then(StagedRow::into_staged)
        .ok_or(PlanIntakeError::WriteReturnedNothing)
}

/// One staged assignment by id.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn find_by_id(
    surreal: &SurrealDb,
    id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    let mut response = surreal
        .query(format!("SELECT {SELECT} FROM ONLY $id"))
        .bind(("id", record_id(TABLE, id)))
        .await?
        .check()?;
    Ok(response
        .take::<Option<StagedRow>>(0)?
        .and_then(StagedRow::into_staged))
}

/// The staged assignment one notice's ingested message produced, or none.
/// Staging's idempotency key.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn find_by_source_message(
    surreal: &SurrealDb,
    source_message_id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE source_message_id = $source_message_id LIMIT 1"
        ))
        .bind(("source_message_id", record_id("message", source_message_id)))
        .await?
        .check()?;
    Ok(response
        .take::<Option<StagedRow>>(0)?
        .and_then(StagedRow::into_staged))
}

/// A firm's staged assignments, newest first. The review screen's queue.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn list_by_firm(
    surreal: &SurrealDb,
    firm_id: Uuid,
) -> Result<Vec<StagedAssignment>, PlanIntakeError> {
    let mut response = surreal
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE firm_id = $firm_id \
             ORDER BY assigned_at DESC LIMIT 100"
        ))
        .bind(("firm_id", record_id("firm", firm_id)))
        .await?
        .check()?;
    let rows: Vec<StagedRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(StagedRow::into_staged)
        .collect())
}

/// Create a fresh `Human` entity for a solo natural person — the same
/// fallback the self-serve retainer walk uses, so a plan member with no
/// pre-existing entity still anchors a matter.
async fn create_human_entity(surreal: &SurrealDb, label: &str) -> Result<Uuid, PlanIntakeError> {
    let type_id = crate::entity_types::find_or_create(surreal, "Human")
        .await?
        .id;
    let jurisdiction_id = crate::jurisdictions::find_or_create(
        surreal,
        &crate::jurisdictions::NewJurisdiction::new("United States", "US", "country"),
    )
    .await?
    .id;
    Ok(crate::entities::create(
        surreal,
        &crate::entities::NewEntity {
            name: label.to_string(),
            entity_type_id: type_id,
            jurisdiction_id,
            phone: None,
            url: None,
            xero_id: None,
            // A solo member's `Human` entity is never the firm anchor.
            firm_anchor_key: None,
        },
    )
    .await?
    .id)
}

/// The notice's state word when it is a valid fee-schedule code, else
/// none — an odd spelling prices nothing rather than poisoning the
/// project's plan columns.
fn sanitized_state(state: Option<&str>) -> Option<String> {
    state
        .filter(|state| crate::legal_plans::valid_state_code(state))
        .map(str::to_string)
}

type ResolvedFee = (String, i32, Option<i32>, Option<i32>, NaiveDate);

/// Resolve the firm's plan for a provider and the fee its schedule quotes.
/// Without PostgreSQL both come back empty — the review screen asks the
/// attorney to pick the plan.
async fn resolve_plan_and_fee(
    pg: Option<&PostgresDb>,
    firm_id: Uuid,
    provider: &str,
    matter_type: PlanMatterType,
    state: Option<&str>,
    on: NaiveDate,
) -> Result<(Option<Uuid>, Option<String>, bool, Option<ResolvedFee>), PlanIntakeError> {
    let Some(pg) = pg else {
        return Ok((None, None, false, None));
    };
    let plans = crate::legal_plans::list(pg, firm_id).await?;
    let Some(plan) = plan_for_provider(&plans, provider) else {
        return Ok((None, None, false, None));
    };
    let fee = match state.filter(|state| crate::legal_plans::valid_state_code(state)) {
        Some(state) => {
            crate::legal_plans::resolve_fee(pg, firm_id, plan.id, matter_type, state, on)
                .await?
                .map(|fee| {
                    (
                        fee.covered_benefit.clone(),
                        fee.plan_fee_cents,
                        fee.supplemental_flat_cents,
                        fee.supplemental_hourly_cents,
                        fee.effective_on,
                    )
                })
        }
        None => None,
    };
    Ok((
        Some(plan.id),
        Some(plan.name.clone()),
        plan.portal.is_some(),
        fee,
    ))
}

/// The plan's SLA deadlines, all internal-authority rows replay-keyed to
/// the staged assignment so a retried flow cannot duplicate them:
///
/// - the acceptance SLA, due from the email-landing time — completed
///   immediately when the acceptance beat it, left open (so the daily
///   alert shows it) when the acceptance missed it;
/// - the turnaround SLA, due from acceptance;
/// - a portal-confirmation reminder when the plan's acceptance is
///   confirmed on the provider's portal instead of by reply email.
async fn create_plan_deadlines(
    surreal: &SurrealDb,
    project_id: Uuid,
    staged: &StagedAssignment,
    plan: &LegalPlan,
    code: &str,
) -> Result<(), PlanIntakeError> {
    let acceptance_due = acceptance_due(staged.assigned_at, plan.acceptance_sla_hours);
    let now = Utc::now();

    let acceptance_deadline = crate::project_deadlines::create(
        surreal,
        &crate::project_deadlines::NewDeadline {
            project_id,
            title: Some("Plan acceptance window".to_string()),
            description: Some(format!(
                "{} expects this assignment accepted within {} hours of the notice \
                 landing. The clock starts at the landing time, not the acceptance.",
                plan.name, plan.acceptance_sla_hours
            )),
            due_at: acceptance_due.to_rfc3339(),
            authority_kind: "internal".to_string(),
            authority: format!("{} acceptance SLA", plan.name),
            source: "plan_intake".to_string(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".to_string(),
            category: Some("internal".to_string()),
            internal_lead_days: None,
            client_lead_days: None,
            replay_key: Some(format!("plan-intake/acceptance-sla/{code}")),
        },
    )
    .await?;
    // An acceptance inside the SLA closes its own clock; a late one stays
    // open and overdue, which is exactly what the daily alert should show.
    if now <= acceptance_due {
        crate::project_deadlines::complete(surreal, project_id, acceptance_deadline.id).await?;
    }

    crate::project_deadlines::create(
        surreal,
        &crate::project_deadlines::NewDeadline {
            project_id,
            title: Some("Plan turnaround deadline".to_string()),
            description: Some(format!(
                "{} expects the matter finished within {} days of acceptance.",
                plan.name, plan.turnaround_sla_days
            )),
            due_at: turnaround_due(now, plan.turnaround_sla_days).to_rfc3339(),
            authority_kind: "internal".to_string(),
            authority: format!("{} turnaround SLA", plan.name),
            source: "plan_intake".to_string(),
            source_document_id: None,
            owner_person_id: None,
            calendar_role: "firm".to_string(),
            category: Some("internal".to_string()),
            internal_lead_days: None,
            client_lead_days: None,
            replay_key: Some(format!("plan-intake/turnaround/{code}")),
        },
    )
    .await?;

    if staged.plan_portal_only {
        crate::project_deadlines::create(
            surreal,
            &crate::project_deadlines::NewDeadline {
                project_id,
                title: Some(format!("Confirm acceptance on the {} portal", plan.name)),
                description: Some(format!(
                    "This plan confirms assignments on its portal ({}) rather than by \
                     reply email. Confirm the acceptance there; the plan's own clock \
                     started when the notice landed.",
                    plan.portal.as_deref().unwrap_or("the provider's portal")
                )),
                due_at: acceptance_due.to_rfc3339(),
                authority_kind: "internal".to_string(),
                authority: format!("{} acceptance SLA", plan.name),
                source: "plan_intake".to_string(),
                source_document_id: None,
                owner_person_id: None,
                calendar_role: "firm".to_string(),
                category: Some("internal".to_string()),
                internal_lead_days: None,
                client_lead_days: None,
                replay_key: Some(format!("plan-intake/portal-confirm/{code}")),
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        accept, acceptance_due, decline, find_by_id, list_by_firm, matter_type_from_notice,
        member_intro_body, plan_for_provider, stage, suggest_code, turnaround_due, AcceptInput,
        PlanIntakeError, StageInput, Status,
    };
    use crate::legal_plans::{LegalPlan, NewLegalPlan, NewPlanFeeSchedule, PlanMatterType};
    use crate::plan_assignment::{fixtures, parse_text};
    use crate::postgres::PostgresDb;
    use crate::test_support::{mem_surreal, seed_firm_with_brand};
    use chrono::{Duration, NaiveDate, TimeZone, Utc};
    use uuid::Uuid;

    /// The ARAG fixture's staged input: a parsed notice with its landing
    /// coordinates.
    fn arag_input(firm_id: Uuid, landing_at: chrono::DateTime<Utc>) -> StageInput {
        StageInput {
            firm_id,
            conversation_id: Uuid::now_v7(),
            source_message_id: Uuid::now_v7(),
            landing_at,
            provider: "arag".to_string(),
            assignment: parse_text(&fixtures::arag_assignment_text())
                .expect("the ARAG fixture parses"),
        }
    }

    /// The LegalShield fixture's staged input.
    fn legal_shield_input(firm_id: Uuid, landing_at: chrono::DateTime<Utc>) -> StageInput {
        StageInput {
            firm_id,
            conversation_id: Uuid::now_v7(),
            source_message_id: Uuid::now_v7(),
            landing_at,
            provider: "legal_shield".to_string(),
            assignment: parse_text(&fixtures::legal_shield_assignment_text())
                .expect("the LegalShield fixture parses"),
        }
    }

    fn arag_plan() -> NewLegalPlan {
        NewLegalPlan {
            name: "ARAG".into(),
            contact: None,
            portal: None,
            acceptance_sla_hours: 24,
            turnaround_sla_days: 10,
            reporting_requirements: None,
        }
    }

    fn schedule(firm_id: Uuid, plan_id: Uuid) -> NewPlanFeeSchedule {
        NewPlanFeeSchedule {
            firm_id,
            legal_plan_id: plan_id,
            matter_type: PlanMatterType::DocumentReview,
            state: "nv".into(),
            covered_benefit: "Will review".into(),
            plan_fee_cents: 15_000,
            supplemental_hourly_cents: Some(25_000),
            supplemental_flat_cents: None,
            effective_on: NaiveDate::from_ymd_opt(2026, 1, 1).expect("calendar date"),
        }
    }

    /// The firm row on PostgreSQL, keyed to the SurrealDB firm a brand was
    /// seeded under — open_matter verifies the brand maps to the same firm
    /// that owns the plan.
    async fn plan_firm(pg: &PostgresDb, firm_id: Uuid) {
        sqlx::query(
            "INSERT INTO firm (id,name,slug,status) \
             VALUES ($1,'Synthetic Plan Firm',$2,'active')",
        )
        .bind(firm_id)
        .bind(format!("pi-{}", Uuid::new_v4().simple()))
        .execute(pg.pool())
        .await
        .expect("firm inserts");
    }

    /// The firm's ARAG plan and its Nevada fee quote.
    async fn arag_plan_on(
        pg: &PostgresDb,
        firm_id: Uuid,
        name: &str,
        portal: Option<&str>,
    ) -> LegalPlan {
        let plan = crate::legal_plans::create(
            pg,
            firm_id,
            &NewLegalPlan {
                name: name.into(),
                portal: portal.map(str::to_string),
                ..arag_plan()
            },
        )
        .await
        .expect("the plan creates");
        crate::legal_plans::add_fee_schedule(pg, &schedule(firm_id, plan.id))
            .await
            .expect("the fee schedule creates");
        plan
    }

    /// A PostgreSQL lane when CI has one; none skips the test.
    async fn postgres() -> Option<PostgresDb> {
        let port = if let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") {
            port.parse::<u16>().unwrap()
        } else {
            assert!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
                "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
            );
            return None;
        };
        let options = sqlx::postgres::PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(sqlx::postgres::PgSslMode::Disable);
        let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
        let database = format!("plan_intake_test_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&admin)
            .await
            .unwrap();
        let pg = PostgresDb::connect(options.database(&database))
            .await
            .unwrap();
        pg.migrate_embedded(None).await.unwrap();
        Some(pg)
    }

    fn accept_input(plan_id: Uuid, attorney_id: Uuid, attestation: bool) -> AcceptInput {
        AcceptInput {
            attestation,
            acting_person_id: attorney_id,
            brand: "plan-brand".to_string(),
            code: "plan-ar-004512".to_string(),
            name: None,
            legal_plan_id: plan_id,
            plan_matter_type: "document_review".to_string(),
            plan_state: Some("nv".to_string()),
            questions: vec!["What is the best number to reach you on?".to_string()],
        }
    }

    #[test]
    fn a_notice_classifies_into_the_closed_plan_vocabulary() {
        assert_eq!(
            matter_type_from_notice("Uncontested Divorce intake"),
            PlanMatterType::UncontestedDivorce
        );
        assert_eq!(
            matter_type_from_notice("Demand Letter review"),
            PlanMatterType::Letter
        );
        assert_eq!(
            matter_type_from_notice("Court appearance coverage"),
            PlanMatterType::CourtAppearance
        );
        assert_eq!(
            matter_type_from_notice("Estate Planning - Will Preparation"),
            PlanMatterType::DocumentReview
        );
        assert_eq!(
            matter_type_from_notice("Consumer Contract Review"),
            PlanMatterType::DocumentReview
        );
    }

    #[test]
    fn a_firms_plan_matches_its_provider_by_name() {
        let plans = vec![
            LegalPlan {
                id: Uuid::now_v7(),
                firm_id: Uuid::now_v7(),
                name: "ARAG group legal".into(),
                contact: None,
                portal: None,
                acceptance_sla_hours: 24,
                turnaround_sla_days: 10,
                reporting_requirements: None,
                inserted_at: Utc::now(),
            },
            LegalPlan {
                id: Uuid::now_v7(),
                firm_id: Uuid::now_v7(),
                name: "LegalShield family plan".into(),
                contact: None,
                portal: None,
                acceptance_sla_hours: 24,
                turnaround_sla_days: 10,
                reporting_requirements: None,
                inserted_at: Utc::now(),
            },
        ];
        assert_eq!(
            plan_for_provider(&plans, "arag").map(|plan| plan.name.as_str()),
            Some("ARAG group legal")
        );
        assert_eq!(
            plan_for_provider(&plans, "legal_shield").map(|plan| plan.name.as_str()),
            Some("LegalShield family plan")
        );
        assert_eq!(plan_for_provider(&plans, "rocket_legal"), None);
    }

    #[test]
    fn the_acceptance_clock_starts_at_landing_and_the_turnaround_at_acceptance() {
        let landing = Utc.with_ymd_and_hms(2026, 11, 9, 14, 0, 0).unwrap();
        assert_eq!(
            acceptance_due(landing, 24),
            Utc.with_ymd_and_hms(2026, 11, 10, 14, 0, 0).unwrap()
        );
        assert_eq!(
            turnaround_due(acceptance_due(landing, 24), 10),
            Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn the_code_suggestion_reduces_the_member_number_to_the_code_vocabulary() {
        assert_eq!(suggest_code("AR-004512"), "plan-ar-004512");
        assert_eq!(suggest_code("ls/902/441 "), "plan-ls-902-441");
        assert_eq!(suggest_code("weird!!"), "plan-weird");
    }

    #[test]
    fn the_member_intro_always_asks_about_sms_and_carries_the_firms_questions() {
        let intro = member_intro_body(
            "Dana Example",
            "Guieb & Shook",
            "uncontested divorce",
            &["What county will you file in?".to_string()],
        );
        assert!(intro.contains("Hello Dana Example,"));
        assert!(intro.contains("uncontested divorce"));
        assert!(intro.contains("What county will you file in?"));
        assert!(intro.contains("may we text you"));
        // No questions means the questionnaire is promised separately, not
        // invented.
        let bare = member_intro_body("Dana Example", "Guieb & Shook", "letter", &[]);
        assert!(bare.contains("intake questionnaire"));
        assert!(!bare.contains('-'));
    }

    #[tokio::test]
    async fn staging_anchors_the_member_and_records_the_conflict_check_pre_write() {
        let surreal = mem_surreal().await;
        let landing = Utc::now();
        let staged = stage(&surreal, None, &arag_input(Uuid::now_v7(), landing))
            .await
            .expect("staging succeeds");

        // The member anchor: a Role::Client person for the notice's
        // contact email, and a fresh entity beside it.
        let person = crate::persons::find_by_email_ci(&surreal, "dana@example.com")
            .await
            .expect("person lookup")
            .expect("the member person exists");
        assert_eq!(staged.member_person_id, Some(person.id));
        assert_eq!(person.role, crate::persons::Role::Client);
        assert!(staged.member_entity_id.is_some());

        // The notice's fields, carried for the review screen.
        assert_eq!(staged.plan_member_id.as_deref(), Some("AR-004512"));
        assert_eq!(staged.plan_number.as_deref(), Some("PLAN-778812"));
        assert_eq!(staged.benefit_code, None);
        assert_eq!(staged.plan_matter_type, "document_review");
        assert_eq!(staged.plan_state.as_deref(), Some("nv"));
        assert_eq!(staged.assigned_at, landing);
        assert_eq!(staged.provider, "arag");

        // The conflict check ran against the anchored member in an empty
        // graph and found nothing blocking.
        assert!(staged.conflict_checked);
        assert!(!staged.conflict_blocking);

        // Staging never opens a matter — no project row exists, and the
        // record is staged with nothing accepted.
        assert_eq!(staged.status, Status::Staged.as_str());
        assert!(staged.project_id.is_none());
        assert!(staged.accepted_by.is_none());
    }

    #[tokio::test]
    async fn staging_is_idempotent_per_source_message() {
        let surreal = mem_surreal().await;
        let input = arag_input(Uuid::now_v7(), Utc::now());
        let first = stage(&surreal, None, &input).await.expect("first stage");
        let second = stage(&surreal, None, &input).await.expect("replay stage");
        assert_eq!(first.id, second.id);
        assert_eq!(second.member_person_id, first.member_person_id);
    }

    #[tokio::test]
    async fn a_notice_without_a_contact_email_stages_unanchored() {
        let surreal = mem_surreal().await;
        let mut input = arag_input(Uuid::now_v7(), Utc::now());
        input.assignment.contact_email = None;
        let staged = stage(&surreal, None, &input)
            .await
            .expect("staging succeeds");
        assert!(staged.member_person_id.is_none());
        assert!(staged.member_entity_id.is_none());
        assert!(!staged.conflict_checked);
        assert!(staged
            .conflict_summary
            .iter()
            .any(|line| line.contains("conflict check has not run")));
    }

    #[tokio::test]
    async fn a_staged_assignment_declines_with_a_reason_without_opening_a_matter() {
        let surreal = mem_surreal().await;
        let staged = stage(&surreal, None, &arag_input(Uuid::now_v7(), Utc::now()))
            .await
            .expect("staging succeeds");
        let declined = decline(
            &surreal,
            staged.id,
            "The member's matter is outside our practice areas.",
        )
        .await
        .expect("decline succeeds");
        assert_eq!(declined.status, Status::Declined.as_str());
        assert!(declined.project_id.is_none());
        assert_eq!(
            declined.decline_reason.as_deref(),
            Some("The member's matter is outside our practice areas.")
        );
        // The member's person and entity remain directory data.
        assert!(staged.member_person_id.is_some());
        // Deciding again refuses.
        assert!(matches!(
            decline(&surreal, staged.id, "again").await,
            Err(PlanIntakeError::NotStaged)
        ));
    }

    #[tokio::test]
    async fn acceptance_refuses_to_open_without_the_attorney_attestation() {
        let Some(pg) = postgres().await else {
            eprintln!("skipping: no PostgreSQL test lane");
            return;
        };
        let surreal = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&surreal, "plan-brand").await;
        plan_firm(&pg, firm_id).await;
        let plan = arag_plan_on(&pg, firm_id, "ARAG", None).await;
        let staged = stage(&surreal, Some(&pg), &arag_input(firm_id, Utc::now()))
            .await
            .expect("staging succeeds");
        let attorney = crate::persons::create(
            &surreal,
            &crate::persons::NewPerson::with_role(
                "Pisces Attorney",
                "plan-intake-attorney@example.com",
                crate::persons::Role::Lawyer,
            ),
        )
        .await
        .expect("the attorney exists");

        // The plan and fee resolved at staging from the firm's schedule.
        assert_eq!(staged.legal_plan_id, Some(plan.id));
        assert_eq!(staged.fee_plan_fee_cents, Some(15_000));

        // Without the attorney's click the open refuses, no matter exists,
        // and the staged record stays staged.
        let refused = accept(
            &surreal,
            &pg,
            staged.id,
            &accept_input(plan.id, attorney.id, false),
        )
        .await
        .expect_err("an unattested acceptance refuses");
        assert!(matches!(
            refused,
            PlanIntakeError::Open(crate::projects::OpenMatterError::AttestationRequired)
        ));
        assert!(
            crate::projects::find_by_code(&surreal, "plan-ar-004512")
                .await
                .expect("project lookup")
                .is_none(),
            "no matter may exist without the attorney's attestation"
        );
        let record = find_by_id(&surreal, staged.id)
            .await
            .expect("record lookup")
            .expect("the record exists");
        assert_eq!(record.status, Status::Staged.as_str());
        assert!(record.project_id.is_none());

        // The parser itself never opens a project either: staging is the
        // only automated step, and it wrote none.
        let staged_again = stage(&surreal, Some(&pg), &arag_input(firm_id, Utc::now()))
            .await
            .expect("restaging is idempotent");
        assert_eq!(staged_again.id, staged.id);
        assert!(staged_again.project_id.is_none());
    }

    #[tokio::test]
    async fn acceptance_opens_the_matter_with_plan_columns_and_deadlines() {
        let Some(pg) = postgres().await else {
            eprintln!("skipping: no PostgreSQL test lane");
            return;
        };
        let surreal = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&surreal, "plan-brand").await;
        plan_firm(&pg, firm_id).await;
        let plan = arag_plan_on(&pg, firm_id, "ARAG", None).await;
        let landing = Utc::now();
        let staged = stage(&surreal, Some(&pg), &arag_input(firm_id, landing))
            .await
            .expect("staging succeeds");
        let attorney = crate::persons::create(
            &surreal,
            &crate::persons::NewPerson::with_role(
                "Pisces Attorney",
                "plan-intake-attorney@example.com",
                crate::persons::Role::Lawyer,
            ),
        )
        .await
        .expect("the attorney exists");

        let accepted = accept(
            &surreal,
            &pg,
            staged.id,
            &accept_input(plan.id, attorney.id, true),
        )
        .await
        .expect("the attested acceptance opens the matter");
        let record = accepted.staged;
        assert_eq!(record.status, Status::Accepted.as_str());
        assert_eq!(record.accepted_by, Some(attorney.id));
        assert_eq!(record.project_id, Some(accepted.project_id));

        // The matter, opened with the member as its client of record and
        // the plan columns filled — the assignment date from the landing,
        // not the acceptance.
        let project = crate::projects::find_by_code(&surreal, "plan-ar-004512")
            .await
            .expect("project lookup")
            .expect("the matter exists");
        assert_eq!(project.id, accepted.project_id);
        let participations = crate::projects::participations_for_project(&surreal, project.id)
            .await
            .expect("project participations");
        assert!(participations.iter().any(|participation| {
            participation.person_id == record.member_person_id.unwrap()
                && crate::projects::PARTICIPATION_CLIENT_SIDE
                    .contains(&participation.participation.as_str())
        }));
        assert_eq!(project.plan_matter_type.as_deref(), Some("document_review"));
        assert_eq!(project.plan_state.as_deref(), Some("nv"));
        assert_eq!(project.plan_member_id.as_deref(), Some("AR-004512"));
        assert_eq!(project.plan_case_number.as_deref(), Some("PLAN-778812"));
        assert_eq!(
            project.plan_assigned_on.as_deref(),
            Some(landing.date_naive().to_string().as_str())
        );
        assert_eq!(
            project.plan_accepted_on.as_deref(),
            Some(Utc::now().date_naive().to_string().as_str())
        );
        assert_eq!(project.legal_plan_id, Some(plan.id));
        // The kit catalog has no plan-matter kit yet: the column stays
        // empty and the flow falls back cleanly.
        assert_eq!(project.kit_version, None);

        // The deadlines: the acceptance clock from the landing instant,
        // the turnaround from acceptance.
        let deadlines = crate::project_deadlines::list(&surreal, project.id)
            .await
            .expect("deadline list");
        assert_eq!(
            deadlines.len(),
            2,
            "an email-plan record gets exactly two SLA deadlines"
        );
        let acceptance = deadlines
            .iter()
            .find(|deadline| deadline.title.as_deref() == Some("Plan acceptance window"))
            .expect("the acceptance deadline exists");
        assert_eq!(acceptance.due_at, acceptance_due(landing, 24).to_rfc3339());
        assert_eq!(
            acceptance.status,
            crate::project_deadlines::STATUS_COMPLETE,
            "an on-time acceptance closes its own clock"
        );
        let turnaround = deadlines
            .iter()
            .find(|deadline| deadline.title.as_deref() == Some("Plan turnaround deadline"))
            .expect("the turnaround deadline exists");
        assert_eq!(
            turnaround.due_at,
            turnaround_due(Utc::now(), 10).to_rfc3339()
        );
        assert_eq!(turnaround.status, "open");

        // The accepted fee quote, from the schedule.
        assert_eq!(record.fee_plan_fee_cents, Some(15_000));
        assert_eq!(record.fee_supplemental_hourly_cents, Some(25_000));
        assert_eq!(record.fee_covered_benefit.as_deref(), Some("Will review"));
    }

    #[tokio::test]
    async fn a_late_acceptance_leaves_the_acceptance_deadline_open_for_the_alert() {
        let Some(pg) = postgres().await else {
            eprintln!("skipping: no PostgreSQL test lane");
            return;
        };
        let surreal = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&surreal, "plan-brand").await;
        plan_firm(&pg, firm_id).await;
        let plan = arag_plan_on(&pg, firm_id, "ARAG", None).await;
        let landing = Utc::now() - Duration::hours(48);
        let staged = stage(&surreal, Some(&pg), &arag_input(firm_id, landing))
            .await
            .expect("staging succeeds");
        let attorney = crate::persons::create(
            &surreal,
            &crate::persons::NewPerson::with_role(
                "Pisces Attorney",
                "plan-intake-attorney@example.com",
                crate::persons::Role::Lawyer,
            ),
        )
        .await
        .expect("the attorney exists");

        let accepted = accept(
            &surreal,
            &pg,
            staged.id,
            &accept_input(plan.id, attorney.id, true),
        )
        .await
        .expect("the attested acceptance opens the matter even though it is late");

        let deadlines = crate::project_deadlines::list(&surreal, accepted.project_id)
            .await
            .expect("deadline list");
        let acceptance = deadlines
            .iter()
            .find(|deadline| deadline.title.as_deref() == Some("Plan acceptance window"))
            .expect("the acceptance deadline exists");
        // The clock is due from the landing, and a late acceptance leaves
        // it open and overdue — exactly what the daily alert should show.
        assert_eq!(acceptance.due_at, acceptance_due(landing, 24).to_rfc3339());
        assert_eq!(acceptance.status, "open");
    }

    #[tokio::test]
    async fn a_portal_only_plan_schedules_the_manual_portal_confirmation() {
        let Some(pg) = postgres().await else {
            eprintln!("skipping: no PostgreSQL test lane");
            return;
        };
        let surreal = mem_surreal().await;
        let firm_id = seed_firm_with_brand(&surreal, "plan-brand").await;
        plan_firm(&pg, firm_id).await;
        let plan = arag_plan_on(&pg, firm_id, "ARAG Portal", Some("portal.arag.example")).await;
        let input = arag_input(firm_id, Utc::now());
        let staged = stage(&surreal, Some(&pg), &input)
            .await
            .expect("staging succeeds");
        assert!(
            staged.plan_portal_only,
            "a plan with a portal accepts on the portal"
        );
        let attorney = crate::persons::create(
            &surreal,
            &crate::persons::NewPerson::with_role(
                "Pisces Attorney",
                "plan-intake-attorney@example.com",
                crate::persons::Role::Lawyer,
            ),
        )
        .await
        .expect("the attorney exists");

        let accepted = accept(
            &surreal,
            &pg,
            staged.id,
            &accept_input(plan.id, attorney.id, true),
        )
        .await
        .expect("the attested acceptance opens the matter");

        let deadlines = crate::project_deadlines::list(&surreal, accepted.project_id)
            .await
            .expect("deadline list");
        assert_eq!(deadlines.len(), 3);
        assert!(deadlines.iter().any(|deadline| deadline
            .title
            .as_deref()
            .is_some_and(|title| title.contains("portal"))));
    }

    #[tokio::test]
    async fn the_review_queue_lists_a_firms_staged_assignments_newest_first() {
        let surreal = mem_surreal().await;
        let firm_id = Uuid::now_v7();
        let other_firm = Uuid::now_v7();
        let older = stage(
            &surreal,
            None,
            &arag_input(firm_id, Utc::now() - Duration::hours(2)),
        )
        .await
        .expect("staging succeeds");
        let newer = stage(&surreal, None, &legal_shield_input(firm_id, Utc::now()))
            .await
            .expect("staging succeeds");
        stage(&surreal, None, &arag_input(other_firm, Utc::now()))
            .await
            .expect("staging succeeds");

        let queue = list_by_firm(&surreal, firm_id).await.expect("queue list");
        assert_eq!(
            queue.iter().map(|staged| staged.id).collect::<Vec<_>>(),
            vec![newer.id, older.id]
        );
        // The detail lookup by id.
        let one = find_by_id(&surreal, older.id)
            .await
            .expect("lookup")
            .expect("the record exists");
        assert_eq!(one.id, older.id);
    }
}
