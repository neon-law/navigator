//! Stage provider notices for the owning Firm. A lawyer confirms the
//! member, reviews conflicts, and takes the referral with an attestation.
//! Taking opens a Pitch, links the Legal Plan and referral identifiers,
//! and records internal SLA Deadlines. Correspondence is a separate action.
//!
//! PostgreSQL holds the queue. The Source projection supports reads
//! and copying for historical rows without assuming a certificate is a case number.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::legal_plans::{LegalPlan, PlanMatterType};
use crate::postgres::PostgresDb;

const TABLE: &str = "plan_intake";

pub(crate) mod postgres;
#[cfg(test)]
pub(crate) mod postgres_tests;

/// A parsed plan assignment held for a person's confirmation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StagedAssignment {
    pub id: Uuid,
    pub firm_id: Uuid,
    /// The Inbox Conversation containing the original notice.
    pub conversation_id: Uuid,
    /// The inbound Message anchoring idempotent staging.
    pub source_message_id: Uuid,
    /// The provider spelling from [`crate::plan_assignment::PlanProvider::as_str`].
    pub provider: String,
    /// The member's name, when the notice prints one.
    pub member_name: Option<String>,
    pub member_email: Option<String>,
    /// The member's anchored client person, when the notice printed a
    /// contact email. `None` leaves the conflict check unrun and refuses
    /// acceptance until a person can anchor the matter.
    pub member_person_id: Option<Uuid>,
    /// A fresh `Human` entity for the member, created beside the person.
    pub member_entity_id: Option<Uuid>,
    /// The notice's member number — the project's `plan_member_id`.
    pub plan_member_id: Option<String>,
    /// The plan's coverage or certificate number, when the notice prints one.
    pub plan_number: Option<String>,
    /// The provider's referral identifier. Older staged rows need verification.
    pub plan_case_number: Option<String>,
    /// The plan's description of the requested work, verbatim.
    pub notice_matter_type: Option<String>,
    /// A benefit code the plan's assignment named, when it ever does.
    pub benefit_code: Option<String>,
    /// The classified Plan Matter Type, or an empty string for an unclassified notice.
    pub plan_matter_type: String,
    /// The lowercased state word from the notice when it survives
    /// [`crate::legal_plans::valid_state_code`].
    pub plan_state: Option<String>,
    /// When the assignment email landed. The acceptance clock starts here,
    /// not when a person reads the notice.
    pub assigned_at: DateTime<Utc>,
    /// The firm's matching plan, resolved from PostgreSQL at staging.
    /// The review checks live Firm configuration before taking the assignment.
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
    take_started_at: Option<DateTime<Utc>>,
    pub decline_reason: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl StagedAssignment {
    /// The predetermined Project ID when a take is in progress.
    #[must_use]
    pub fn pending_project_id(&self) -> Option<Uuid> {
        self.take_started_at.map(|_| self.id)
    }
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
    /// The case number the lawyer confirmed against an older notice.
    pub plan_case_number: Option<String>,
    /// The matter type to work the matter as, correctable on the screen.
    pub plan_matter_type: String,
    /// The jurisdiction to price and work the matter under, correctable.
    pub plan_state: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanIntakeError {
    /// The configured PostgreSQL server could not be reached.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] crate::postgres::PostgresError),
    /// Source reads cannot stage or decide a referral.
    #[error("Plan Intake writes require PostgreSQL authority")]
    PostgresRequired,
    /// A PostgreSQL statement failed.
    #[error("PostgreSQL query: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("writing a staged assignment returned no usable row")]
    WriteReturnedNothing,
    #[error("unknown staged assignment")]
    NotFound,
    #[error("the assignment is no longer staged")]
    NotStaged,
    #[error("Confirm the member and review the conflict result before taking this assignment.")]
    MissingMemberAnchor,
    #[error("The provider must match exactly one Legal Plan configured for this Firm.")]
    PlanNotFound,
    #[error("Confirm the provider's case number from the original notice.")]
    MissingCaseNumber,
    #[error("A Project is already linked to this provider case, or it is being opened from another notice.")]
    DuplicateCase,
    #[error("Enter the member's name and verified email address.")]
    InvalidMember,
    #[error("That email does not identify the named client. Confirm the member's own email.")]
    ConflictingMember,
    #[error(transparent)]
    Firm(#[from] crate::firms::FirmError),
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
    #[error("start Legal Plan Onboarding: {0}")]
    Onboarding(String),
}

/// Classify a notice's matter-type words into the closed plan vocabulary.
/// The review screen shows the classification beside the notice's own
/// words so the attorney can correct it before acceptance.
#[must_use]
pub fn matter_type_from_notice(text: &str) -> Option<PlanMatterType> {
    let lowered = text.to_ascii_lowercase();
    let words: Vec<_> = lowered
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    if words.contains(&"divorce") {
        if words.contains(&"contested") {
            return Some(PlanMatterType::ContestedDivorce);
        }
        return (words
            .windows(2)
            .any(|pair| pair == ["uncontested", "divorce"])
            && !words.contains(&"custody"))
        .then_some(PlanMatterType::UncontestedDivorce);
    }
    if words.contains(&"probate") {
        return Some(PlanMatterType::Probate);
    }
    if words.contains(&"custody") {
        return Some(PlanMatterType::Custody);
    }
    if words.contains(&"contested") {
        return None;
    }
    if words == ["letter"]
        || words
            .windows(2)
            .any(|pair| matches!(pair, ["demand" | "draft", "letter"]))
    {
        Some(PlanMatterType::Letter)
    } else if words
        .windows(2)
        .any(|pair| matches!(pair, ["court", "appearance" | "hearing"]))
    {
        Some(PlanMatterType::CourtAppearance)
    } else if words
        .windows(2)
        .any(|pair| matches!(pair, ["document" | "contract" | "agreement", "review"]))
    {
        Some(PlanMatterType::DocumentReview)
    } else {
        None
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
        PlanMatterType::ContestedDivorce => "contested divorce",
        PlanMatterType::Custody => "custody",
        PlanMatterType::Probate => "probate",
    }
}

/// The owning Firm's unique plan matching the provider's name.
#[must_use]
pub fn plan_for_provider<'a>(plans: &'a [LegalPlan], provider: &str) -> Option<&'a LegalPlan> {
    let mut matches = plans.iter().filter(|plan| {
        crate::plan_assignment::provider_for_plan_name(&plan.name)
            .is_some_and(|matched| matched.as_str() == provider)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// The acceptance deadline's due instant: the plan's acceptance SLA
/// measured from the moment the assignment email landed — never from
/// when a person happened to read or accept it.
#[must_use]
pub fn acceptance_due(landing: DateTime<Utc>, acceptance_sla_hours: Option<i32>) -> DateTime<Utc> {
    acceptance_sla_hours.map_or_else(
        || crate::firm_digest::next_business_day(landing),
        |hours| landing + Duration::hours(i64::from(hours)),
    )
}

/// The turnaround deadline's due instant: the plan's turnaround SLA
/// measured from acceptance.
#[must_use]
pub fn turnaround_due(accepted_at: DateTime<Utc>, turnaround_sla_days: i32) -> DateTime<Utc> {
    accepted_at + Duration::days(i64::from(turnaround_sla_days))
}

/// Suggest a code from the member's surname, requested work, and state.
#[must_use]
pub fn suggest_code(member: &str, request: Option<&str>, state: Option<&str>) -> String {
    let Some(surname) = member.split_whitespace().next_back() else {
        return String::new();
    };
    [Some(surname), request.or(Some("plan")), state]
        .into_iter()
        .flatten()
        .flat_map(|part| part.split(|c: char| !c.is_ascii_alphanumeric()))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(crate::projects::PROJECT_CODE_MAX_LEN)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned()
}

/// The matter-name prefill for a staged assignment.
#[must_use]
pub fn default_matter_name(
    plan_name: &str,
    member_name: &str,
    matter_type: Option<PlanMatterType>,
) -> String {
    let name = format!("{plan_name} plan matter — {member_name}");
    match matter_type {
        Some(kind) => format!("{name} ({})", matter_label(kind)),
        None => name,
    }
}

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
    application_store: &ApplicationStore,
    pg: Option<&PostgresDb>,
    input: &StageInput,
    matter_type: Option<PlanMatterType>,
) -> Result<StagingContext, PlanIntakeError> {
    let assignment = &input.assignment;
    let member = anchor_member(application_store, assignment).await?;
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
        conflict_report(application_store, member).await?;

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
/// without a contact email or a printed name stages unanchored; the
/// review screen resolves the member before acceptance.
async fn anchor_member(
    application_store: &ApplicationStore,
    assignment: &crate::plan_assignment::Assignment,
) -> Result<Option<(Uuid, Uuid)>, PlanIntakeError> {
    let Some(email) = &assignment.contact_email else {
        return Ok(None);
    };
    // Anchoring a Navigator person needs a name; a notice that printed
    // only an email stages unanchored for the attorney to resolve.
    let Some(member_name) = &assignment.member_name else {
        return Ok(None);
    };
    let person = member_person(application_store, member_name, email).await?;
    if !same_member_name(&person.name, member_name) {
        return Ok(None);
    }
    let entity = create_human_entity(application_store, member_name).await?;
    Ok(Some((person.id, entity)))
}

async fn member_person(
    application_store: &ApplicationStore,
    name: &str,
    email: &str,
) -> Result<crate::persons::Person, PlanIntakeError> {
    let email = email.trim().to_ascii_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err(PlanIntakeError::InvalidMember);
    };
    if name.trim().is_empty()
        || local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.chars().any(char::is_whitespace)
        || crate::plan_assignment::provider_for_sender(&email).is_some()
    {
        return Err(PlanIntakeError::InvalidMember);
    }
    let person = match crate::persons::find_by_any_email(application_store, &email).await? {
        Some(person) => person,
        None => {
            crate::persons::find_or_create(
                application_store,
                &crate::persons::NewPerson::with_role(
                    name.trim(),
                    email,
                    crate::persons::Role::Client,
                ),
            )
            .await?
        }
    };
    if person.role != crate::persons::Role::Client {
        return Err(PlanIntakeError::ConflictingMember);
    }
    Ok(person)
}

/// The pre-write conflict report for the confirmed member.
async fn conflict_report(
    application_store: &ApplicationStore,
    member: Option<(Uuid, Uuid)>,
) -> Result<(Vec<String>, bool, bool), PlanIntakeError> {
    match member {
        Some((person_id, entity_id)) => {
            let report =
                crate::conflicts::check_new_matter(application_store, person_id, entity_id)
                    .await
                    .map_err(PlanIntakeError::Conflict)?;
            Ok((report.summary_lines(), report.has_blocking(), true))
        }
        None => Ok((Vec::new(), false, false)),
    }
}

async fn write_authority() -> Result<PostgresDb, PlanIntakeError> {
    Ok(PostgresDb::application().await?)
}

/// Stage one parsed assignment for a person's confirmation. Idempotent per
/// `StageInput::source_message_id`: a redelivered notice lands on the
/// already-staged record instead of a second row. Never opens a matter —
/// a matter exists only through [`accept`].
///
/// `pg` supplies the optional plan and fee lookup. Without it those fields
/// stay empty for attorney review. Staging requires PostgreSQL authority.
///
/// # Errors
///
/// [`PlanIntakeError`] for database or reference-data failures, or a
/// failing conflict check.
pub async fn stage(
    application_store: &ApplicationStore,
    pg: Option<&PostgresDb>,
    input: &StageInput,
) -> Result<StagedAssignment, PlanIntakeError> {
    let authority = write_authority().await?;
    if let Some(existing) =
        find_by_source_message(application_store, input.source_message_id).await?
    {
        return Ok(existing);
    }

    let assignment = &input.assignment;
    let matter_type = matter_type_from_notice(assignment.matter_type.as_deref().unwrap_or(""));
    let context = staging_context(application_store, pg, input, matter_type).await?;
    let now = Utc::now();
    let (fee_covered_benefit, fee_plan_fee_cents, flat, hourly, fee_effective_on) =
        match context.fee {
            Some((benefit, plan_fee, flat, hourly, effective_on)) => (
                Some(benefit),
                Some(plan_fee),
                flat,
                hourly,
                Some(effective_on),
            ),
            None => (None, None, None, None, None),
        };
    let row = StagedAssignment {
        id: Uuid::now_v7(),
        firm_id: input.firm_id,
        conversation_id: input.conversation_id,
        source_message_id: input.source_message_id,
        provider: input.provider.clone(),
        member_name: assignment.member_name.clone(),
        member_email: assignment.contact_email.clone(),
        member_person_id: context.member_person_id,
        member_entity_id: context.member_entity_id,
        plan_member_id: assignment.member_number.clone(),
        benefit_code: None,
        plan_number: assignment.plan_number.clone(),
        plan_case_number: Some(assignment.case_number.clone()),
        notice_matter_type: assignment.matter_type.clone(),
        plan_matter_type: matter_type.map_or("", PlanMatterType::as_str).to_string(),
        plan_state: sanitized_state(assignment.state.as_deref()),
        assigned_at: input.landing_at,
        legal_plan_id: context.legal_plan_id,
        legal_plan_name: context.legal_plan_name,
        plan_portal_only: context.plan_portal_only,
        fee_covered_benefit,
        fee_plan_fee_cents,
        fee_supplemental_flat_cents: flat,
        fee_supplemental_hourly_cents: hourly,
        fee_effective_on,
        conflict_summary: context.conflict_summary,
        conflict_blocking: context.conflict_blocking,
        conflict_checked: context.conflict_checked,
        status: Status::Staged.as_str().to_string(),
        project_id: None,
        accepted_by: None,
        decided_at: None,
        take_started_at: None,
        decline_reason: None,
        inserted_at: now,
        updated_at: now,
    };
    Box::pin(postgres::stage(&authority, &row)).await
}

/// The accepted state of one staged assignment: the opened matter's id
/// and the record carrying the plan columns as filed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub staged: StagedAssignment,
    pub project_id: Uuid,
}

/// The Firm whose intake queue a lawyer may review on the current Brand.
///
/// # Errors
///
/// Returns an error when the actor is outside the Brand's Firm or a lookup fails.
pub async fn review_firm(
    application_store: &ApplicationStore,
    actor_id: Uuid,
    brand: &str,
) -> Result<Uuid, PlanIntakeError> {
    use crate::firms::FirmMembership;
    use crate::persons::Role;

    let person = crate::persons::find_by_id(application_store, actor_id)
        .await?
        .ok_or(PlanIntakeError::NotFound)?;
    let firm = crate::firms::firm_id_for_brand_key(application_store, brand)
        .await?
        .ok_or(PlanIntakeError::NotFound)?;
    if person.role == Role::Owner {
        return Ok(firm);
    }
    if person.role.is_lawyer_tier()
        && crate::firms::membership_for_person(application_store, actor_id, firm)
            .await?
            .is_some_and(|role| {
                matches!(
                    role.membership,
                    FirmMembership::Admin | FirmMembership::Lawyer
                )
            })
    {
        return Ok(firm);
    }
    Err(PlanIntakeError::NotFound)
}

async fn scoped_assignment(
    application_store: &ApplicationStore,
    staged_id: Uuid,
    actor_id: Uuid,
    brand: &str,
) -> Result<StagedAssignment, PlanIntakeError> {
    let firm = review_firm(application_store, actor_id, brand).await?;
    find_by_id(application_store, staged_id)
        .await?
        .filter(|row| row.firm_id == firm)
        .ok_or(PlanIntakeError::NotFound)
}

/// Confirm the member's email before asking the lawyer to attest to the
/// conflict result. An identified member stays fixed on that staged notice.
///
/// # Errors
///
/// Returns an error when the row is outside the Firm, already decided, or
/// the verified email cannot identify the named client.
pub async fn resolve_member(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    staged_id: Uuid,
    actor_id: Uuid,
    brand: &str,
    name: Option<&str>,
    email: &str,
) -> Result<(), PlanIntakeError> {
    write_authority().await?;
    let _lock = postgres::lock(pg, &format!("plan-intake/{staged_id}")).await?;
    let mut row = scoped_assignment(application_store, staged_id, actor_id, brand).await?;
    if row.status != Status::Staged.as_str() || row.take_started_at.is_some() {
        return Err(PlanIntakeError::NotStaged);
    }
    let member =
        if let (Some(person_id), Some(entity_id)) = (row.member_person_id, row.member_entity_id) {
            if !row
                .member_email
                .as_deref()
                .is_some_and(|confirmed| confirmed.eq_ignore_ascii_case(email.trim()))
            {
                return Err(PlanIntakeError::ConflictingMember);
            }
            if row.conflict_checked {
                return Ok(());
            }
            (person_id, entity_id)
        } else {
            let name = row
                .member_name
                .as_deref()
                .or(name)
                .ok_or(PlanIntakeError::InvalidMember)?;
            let person = member_person(application_store, name, email).await?;
            if !same_member_name(&person.name, name) {
                return Err(PlanIntakeError::ConflictingMember);
            }
            let entity_id = create_human_entity(application_store, name).await?;
            row.member_name = Some(name.to_owned());
            row.member_email = Some(email.trim().to_ascii_lowercase());
            row.member_person_id = Some(person.id);
            row.member_entity_id = Some(entity_id);
            // Keep the anchor if the graph is temporarily unavailable.
            // Taking still requires the conflict check to complete.
            postgres::save_member(pg, &row).await?;
            (person.id, entity_id)
        };
    let (summary, blocking, checked) = conflict_report(application_store, Some(member)).await?;
    row.conflict_summary = summary;
    row.conflict_blocking = blocking;
    row.conflict_checked = checked;
    postgres::save_member(pg, &row).await
}

fn same_member_name(left: &str, right: &str) -> bool {
    left.split_whitespace()
        .map(str::to_lowercase)
        .eq(right.split_whitespace().map(str::to_lowercase))
}

fn validated_matter_type(input: &AcceptInput) -> Result<Option<PlanMatterType>, PlanIntakeError> {
    let matter_type = if input.plan_matter_type.is_empty() {
        None
    } else {
        Some(
            PlanMatterType::parse(&input.plan_matter_type)
                .map_err(|_| PlanIntakeError::InvalidMatterType)?,
        )
    };
    if let Some(state) = &input.plan_state {
        if !crate::legal_plans::valid_state_code(state) {
            return Err(PlanIntakeError::InvalidState);
        }
    }
    Ok(matter_type)
}

fn confirmed_case<'a>(
    row: &'a StagedAssignment,
    input: &'a AcceptInput,
) -> Result<&'a str, PlanIntakeError> {
    if let Some(number) = row.plan_case_number.as_deref() {
        return Ok(number);
    }
    input
        .plan_case_number
        .as_deref()
        .map(str::trim)
        .filter(|number| number.bytes().any(|byte| byte.is_ascii_digit()))
        .filter(|number| {
            !row.plan_member_id
                .as_deref()
                .is_some_and(|member| member.eq_ignore_ascii_case(number))
        })
        .filter(|number| {
            row.provider == "legal_shield"
                || !row
                    .plan_number
                    .as_deref()
                    .is_some_and(|certificate| certificate.eq_ignore_ascii_case(number))
        })
        .ok_or(PlanIntakeError::MissingCaseNumber)
}

async fn open_on_acceptance(
    application_store: &ApplicationStore,
    staged: &StagedAssignment,
    plan: &LegalPlan,
    person_id: Uuid,
    entity_id: Uuid,
    input: &AcceptInput,
    matter_type: Option<PlanMatterType>,
) -> Result<crate::projects::Project, PlanIntakeError> {
    let name = input.name.clone().unwrap_or_else(|| {
        default_matter_name(
            &plan.name,
            staged.member_name.as_deref().unwrap_or("the member"),
            matter_type,
        )
    });
    Ok(crate::projects::open_matter_for_intake(
        application_store,
        &crate::projects::OpenMatterCommand {
            firm_id: Some(staged.firm_id),
            name,
            code: input.code.clone(),
            client_id: person_id,
            entity_id,
            jurisdiction_id: None,
            description: None,
            brand: input.brand.clone(),
            attestation: input.attestation,
            acting_person_id: input.acting_person_id,
            closed_at: None,
        },
        staged.id,
    )
    .await?)
}

/// Mark the record accepted with the plan columns as filed and return the
/// updated row.
async fn mark_accepted(
    pg: &PostgresDb,
    staged: &StagedAssignment,
    project_id: Uuid,
    input: &AcceptInput,
    plan: &crate::legal_plans::LegalPlan,
    fee: Option<crate::legal_plans::PlanFeeSchedule>,
    accepted_at: DateTime<Utc>,
) -> Result<StagedAssignment, PlanIntakeError> {
    let matter_type = validated_matter_type(input)?;
    let mut accepted = staged.clone();
    accepted.status = Status::Accepted.as_str().to_string();
    accepted.project_id = Some(project_id);
    accepted.accepted_by = Some(input.acting_person_id);
    accepted.decided_at = Some(accepted_at);
    accepted.plan_matter_type = matter_type.map_or("", PlanMatterType::as_str).to_string();
    accepted.plan_state.clone_from(&input.plan_state);
    accepted.legal_plan_id = Some(plan.id);
    accepted.legal_plan_name = Some(plan.name.clone());
    accepted.plan_portal_only = plan.portal.is_some();
    accepted.fee_covered_benefit = fee.as_ref().map(|fee| fee.covered_benefit.clone());
    accepted.fee_plan_fee_cents = fee.as_ref().map(|fee| fee.plan_fee_cents);
    accepted.fee_supplemental_flat_cents = fee.as_ref().and_then(|fee| fee.supplemental_flat_cents);
    accepted.fee_supplemental_hourly_cents =
        fee.as_ref().and_then(|fee| fee.supplemental_hourly_cents);
    accepted.fee_effective_on = fee.as_ref().map(|fee| fee.effective_on);
    accepted.updated_at = accepted_at;
    Box::pin(postgres::decide(pg, &accepted)).await
}

/// Accept a staged assignment: open the matter on the attorney's
/// attestation, fill its plan columns (with the email-landing date as
/// `plan_assigned_on`), create the plan's SLA deadlines, and mark the
/// record accepted. The attestation is forwarded from the caller —
/// [`crate::projects::open_matter`] refuses to open without it, so no
/// path through this function opens a matter without the attorney's click.
///
/// A retry of the same staged notice returns the existing accepted Project.
///
/// # Errors
///
/// Returns an error for an inaccessible or declined notice, an unresolved
/// member or Legal Plan, or a Project open that fails its checks.
pub async fn accept(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    staged_id: Uuid,
    input: &AcceptInput,
) -> Result<Accepted, PlanIntakeError> {
    accept_with_onboarding(application_store, pg, staged_id, input, |_| async {
        Ok(())
    })
    .await
}

/// The verified referral and fee schedule provided to the onboarding step.
pub struct PlanOnboarding {
    pub provider: String,
    pub project_id: Uuid,
    pub person_id: Uuid,
    pub fee: Option<crate::legal_plans::PlanFeeSchedule>,
}

/// Take a referral and start its onboarding under the Project reservation.
///
/// The onboarding step runs before the intake is recorded as accepted. A
/// failure leaves the same Project reserved for retry. An accepted referral
/// returns its existing Project without reopening onboarding.
///
/// # Errors
/// Returns the same refusals as [`accept`] or a failure to start onboarding.
#[expect(
    clippy::too_many_lines,
    reason = "the ordered take steps share one reservation and its retry boundary"
)]
pub async fn accept_with_onboarding<F>(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    staged_id: Uuid,
    input: &AcceptInput,
    start_onboarding: impl FnOnce(PlanOnboarding) -> F,
) -> Result<Accepted, PlanIntakeError>
where
    F: std::future::Future<Output = Result<(), PlanIntakeError>> + Send,
{
    if !input.attestation {
        return Err(crate::projects::OpenMatterError::AttestationRequired.into());
    }
    write_authority().await?;
    let mut lock = postgres::lock(pg, &format!("plan-intake/{staged_id}")).await?;
    let mut staged = scoped_assignment(
        application_store,
        staged_id,
        input.acting_person_id,
        &input.brand,
    )
    .await?;
    if staged.status == Status::Accepted.as_str() {
        if staged.accepted_by != Some(input.acting_person_id) {
            return Err(PlanIntakeError::NotFound);
        }
        let project_id = staged
            .project_id
            .ok_or(PlanIntakeError::WriteReturnedNothing)?;
        return Ok(Accepted { staged, project_id });
    }
    if staged.status != Status::Staged.as_str() {
        return Err(PlanIntakeError::NotStaged);
    }
    let person_id = staged
        .member_person_id
        .ok_or(PlanIntakeError::MissingMemberAnchor)?;
    let entity_id = staged
        .member_entity_id
        .ok_or(PlanIntakeError::MissingMemberAnchor)?;
    if !staged.conflict_checked {
        return Err(PlanIntakeError::MissingMemberAnchor);
    }
    if staged.conflict_blocking {
        return Err(
            crate::projects::OpenMatterError::BlockingConflict(staged.conflict_summary).into(),
        );
    }
    let matter_type = validated_matter_type(input)?;
    let case_number = confirmed_case(&staged, input)?.to_owned();
    if !crate::projects::valid_plan_case_number(&case_number) {
        return Err(PlanIntakeError::MissingCaseNumber);
    }
    let plans = crate::legal_plans::list(pg, staged.firm_id).await?;
    let plan = plan_for_provider(&plans, &staged.provider).ok_or(PlanIntakeError::PlanNotFound)?;
    let accepted_at = staged.take_started_at.unwrap_or_else(Utc::now);
    postgres::lock_in(
        &mut lock,
        &format!(
            "plan-case/{}/{}/{}",
            staged.firm_id,
            staged.provider,
            case_number.to_ascii_lowercase()
        ),
    )
    .await?;
    let code = crate::projects::normalize_code(&input.code).ok_or(
        crate::projects::OpenMatterError::Invalid("Project code is required."),
    )?;
    if let Some(existing) = crate::projects::find_by_code(application_store, &code).await? {
        if existing.id != staged.id {
            return Err(crate::projects::OpenMatterError::CodeConflict.into());
        }
    }
    postgres::reserve_case(pg, &staged, plan.id, &case_number, accepted_at).await?;
    staged.plan_case_number = Some(case_number.clone());
    staged.plan_portal_only = plan.portal.is_some();

    let project = match open_on_acceptance(
        application_store,
        &staged,
        plan,
        person_id,
        entity_id,
        input,
        matter_type,
    )
    .await
    {
        Ok(project) => project,
        Err(error) => {
            // A successful open may be followed by an attestation write
            // failure. Keep that Project's reservation for the next retry.
            if crate::projects::find_by_id(application_store, staged.id)
                .await?
                .is_none()
            {
                postgres::release_case(pg, staged.id).await?;
            }
            return Err(error);
        }
    };
    crate::projects::set_plan_columns(
        application_store,
        pg,
        project.id,
        &crate::projects::PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_member_id: staged.plan_member_id.clone(),
            plan_case_number: Some(case_number),
            benefit_code: staged.benefit_code.clone(),
            plan_matter_type: matter_type.map(|kind| kind.as_str().to_owned()),
            plan_state: input.plan_state.clone(),
            // The assignment date is the email-landing date, not the
            // acceptance date — the plan's clock started at landing.
            plan_assigned_on: Some(staged.assigned_at.date_naive().to_string()),
            plan_accepted_on: Some(accepted_at.date_naive().to_string()),
            plan_closed_on: None,
            // The kit catalog is compiled from the project-skill kits;
            // until ENG-1271's catalog matches plan matter types, no kit
            // version applies and the column stays empty.
            kit_version: None,
        },
    )
    .await?;

    // The quote as accepted, from the corrected matter type and state.
    let fee = if let (Some(matter_type), Some(state)) = (matter_type, &input.plan_state) {
        crate::legal_plans::resolve_fee(
            pg,
            staged.firm_id,
            plan.id,
            matter_type,
            state,
            staged
                .fee_effective_on
                .unwrap_or_else(|| staged.assigned_at.date_naive()),
        )
        .await?
    } else {
        None
    };

    // The plan's SLA deadlines, from the email-landing time.
    create_plan_deadlines(
        application_store,
        project.id,
        &staged,
        plan,
        &project.code,
        accepted_at,
    )
    .await?;

    start_onboarding(PlanOnboarding {
        provider: staged.provider.clone(),
        project_id: project.id,
        person_id,
        fee: fee.clone(),
    })
    .await?;

    let staged = mark_accepted(pg, &staged, project.id, input, plan, fee, accepted_at).await?;
    Ok(Accepted {
        staged,
        project_id: project.id,
    })
}

/// Record the lawyer's decision within the same scope and lock as taking it.
///
/// # Errors
///
/// Returns an error when the row is outside the Firm, a Project has opened,
/// the assignment was already decided, or a lookup or write fails.
pub async fn decline_from_review(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    staged_id: Uuid,
    actor_id: Uuid,
    brand: &str,
    reason: &str,
) -> Result<StagedAssignment, PlanIntakeError> {
    write_authority().await?;
    let _lock = postgres::lock(pg, &format!("plan-intake/{staged_id}")).await?;
    let row = scoped_assignment(application_store, staged_id, actor_id, brand).await?;
    if crate::projects::find_by_id(application_store, row.id)
        .await?
        .is_some()
    {
        return Err(PlanIntakeError::NotStaged);
    }
    if row.take_started_at.is_some() {
        postgres::release_case(pg, staged_id).await?;
    }
    decline(application_store, staged_id, reason).await
}

/// Decline a staged assignment without opening a matter. The record and
/// its conflict report stay auditable; the member's person and entity rows
/// remain — they are directory data, not matter data.
///
/// # Errors
///
/// [`PlanIntakeError::NotStaged`] when the record was already decided.
pub(crate) async fn decline(
    application_store: &ApplicationStore,
    staged_id: Uuid,
    reason: &str,
) -> Result<StagedAssignment, PlanIntakeError> {
    let authority = write_authority().await?;
    let staged = find_by_id(application_store, staged_id)
        .await?
        .ok_or(PlanIntakeError::NotFound)?;
    if staged.status != Status::Staged.as_str() || staged.take_started_at.is_some() {
        return Err(PlanIntakeError::NotStaged);
    }
    let now = Utc::now();
    let mut declined = staged;
    declined.status = Status::Declined.as_str().to_string();
    declined.decided_at = Some(now);
    declined.decline_reason = Some(reason.trim().to_string());
    declined.updated_at = now;
    Box::pin(postgres::decide(&authority, &declined)).await
}

/// One staged assignment by id.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn find_by_id(
    _application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::find_by_id(&pg, id)).await
}

/// The staged assignment one notice's ingested message produced, or none.
/// Staging's idempotency key.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn find_by_source_message(
    _application_store: &ApplicationStore,
    source_message_id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::find_by_source_message(&pg, source_message_id)).await
}

/// A firm's staged assignments, newest first. The review screen's queue.
///
/// # Errors
///
/// [`PlanIntakeError`] for database failures.
pub async fn list_by_firm(
    _application_store: &ApplicationStore,
    firm_id: Uuid,
) -> Result<Vec<StagedAssignment>, PlanIntakeError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::list_by_firm(&pg, firm_id)).await
}

/// Create a fresh `Human` entity for a solo natural person — the same
/// fallback the self-serve retainer walk uses, so a plan member with no
/// pre-existing entity still anchors a matter.
async fn create_human_entity(
    application_store: &ApplicationStore,
    label: &str,
) -> Result<Uuid, PlanIntakeError> {
    let type_id = crate::entity_types::find_or_create(application_store, "Human")
        .await?
        .id;
    let jurisdiction_id = crate::jurisdictions::find_or_create(
        application_store,
        &crate::jurisdictions::NewJurisdiction::new("United States", "US", "country"),
    )
    .await?
    .id;
    Ok(crate::entities::create(
        application_store,
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
    matter_type: Option<PlanMatterType>,
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
    let fee = match (
        matter_type,
        state.filter(|state| crate::legal_plans::valid_state_code(state)),
    ) {
        (Some(matter_type), Some(state)) => {
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
        _ => None,
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
    application_store: &ApplicationStore,
    project_id: Uuid,
    staged: &StagedAssignment,
    plan: &LegalPlan,
    code: &str,
    accepted_at: DateTime<Utc>,
) -> Result<(), PlanIntakeError> {
    let acceptance_due = acceptance_due(staged.assigned_at, plan.acceptance_sla_hours);

    let acceptance_deadline = crate::project_deadlines::create(
        application_store,
        &crate::project_deadlines::NewDeadline {
            project_id,
            title: Some("Plan acceptance window".to_string()),
            description: Some(format!(
                "Review this assignment by {} for {}.",
                acceptance_due, plan.name
            )),
            hearing: None,
            effort_minutes: None,
            due_on: None,
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
    if accepted_at <= acceptance_due {
        crate::project_deadlines::complete(application_store, project_id, acceptance_deadline.id)
            .await?;
    }

    if let Some(days) = plan.turnaround_sla_days {
        crate::project_deadlines::create(
            application_store,
            &crate::project_deadlines::NewDeadline {
                project_id,
                title: Some("Plan turnaround deadline".to_string()),
                description: Some(format!(
                    "{} expects the matter finished within {} days of acceptance.",
                    plan.name, days
                )),
                hearing: None,
                effort_minutes: None,
                due_on: None,
                due_at: turnaround_due(accepted_at, days).to_rfc3339(),
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
    }

    if staged.plan_portal_only {
        crate::project_deadlines::create(
            application_store,
            &crate::project_deadlines::NewDeadline {
                project_id,
                title: Some(format!("Confirm acceptance on the {} portal", plan.name)),
                description: Some(format!(
                    "This plan confirms assignments on its portal ({}) rather than by \
                     reply email. Confirm the acceptance there; the plan's own clock \
                     started when the notice landed.",
                    plan.portal.as_deref().unwrap_or("the provider's portal")
                )),
                hearing: None,
                effort_minutes: None,
                due_on: None,
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
        acceptance_due, matter_type_from_notice, plan_for_provider, suggest_code, turnaround_due,
    };
    use crate::legal_plans::{LegalPlan, PlanMatterType};

    use chrono::{Duration, TimeZone, Utc};
    use uuid::Uuid;

    #[test]
    fn classification_only_suggests_a_supported_type_with_explicit_evidence() {
        for notice in [
            "Divorce",
            "Uncontested Divorce - with custody",
            "Estate Planning - Will Preparation",
            "",
        ] {
            assert_eq!(
                matter_type_from_notice(notice).map(PlanMatterType::as_str),
                None
            );
        }
        for (notice, expected) in [
            ("Uncontested Divorce intake", "uncontested_divorce"),
            ("Demand Letter review", "letter"),
            ("Court appearance coverage", "court_appearance"),
            ("Consumer Contract Review", "document_review"),
            (
                "Contested Divorce/Separation/Annulment - with custody",
                "contested_divorce",
            ),
            ("Custody hearing", "custody"),
            ("Probate", "probate"),
            ("Probate court appearance", "probate"),
        ] {
            assert_eq!(
                matter_type_from_notice(notice).map(PlanMatterType::as_str),
                Some(expected)
            );
        }
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
                acceptance_sla_hours: Some(24),
                turnaround_sla_days: Some(10),
                reporting_requirements: None,
                inserted_at: Utc::now(),
            },
            LegalPlan {
                id: Uuid::now_v7(),
                firm_id: Uuid::now_v7(),
                name: "LegalShield family plan".into(),
                contact: None,
                portal: None,
                acceptance_sla_hours: Some(24),
                turnaround_sla_days: Some(10),
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
    fn the_default_response_is_due_on_the_next_weekday() {
        let monday = Utc.with_ymd_and_hms(2026, 11, 9, 15, 30, 0).unwrap();
        for day in [6, 7, 8] {
            let arrival = Utc.with_ymd_and_hms(2026, 11, day, 15, 30, 0).unwrap();
            assert_eq!(acceptance_due(arrival, None), monday);
        }
        assert_eq!(acceptance_due(monday, None), monday + Duration::days(1));
    }

    #[test]
    fn the_acceptance_clock_starts_at_landing_and_the_turnaround_at_acceptance() {
        let landing = Utc.with_ymd_and_hms(2026, 11, 9, 14, 0, 0).unwrap();
        assert_eq!(
            acceptance_due(landing, Some(24)),
            Utc.with_ymd_and_hms(2026, 11, 10, 14, 0, 0).unwrap()
        );
        assert_eq!(
            turnaround_due(acceptance_due(landing, Some(24)), 10),
            Utc.with_ymd_and_hms(2026, 11, 20, 14, 0, 0).unwrap()
        );
    }

    #[test]
    fn the_code_suggestion_uses_the_surname_requested_work_and_state() {
        assert_eq!(
            suggest_code("Dana Example", Some("Lease / Agreement"), Some("NV")),
            "example-lease-agreement-nv"
        );
        assert_eq!(suggest_code("Dana Sample", None, None), "sample-plan");
        assert_eq!(suggest_code("", None, None), "");
    }
}
