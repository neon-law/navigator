//! Project (matter) lifecycle helpers.
//!
//! A matter's `status` walks `open` → `closed` → `archived`
//! (`Project::status`). Opening is done at retainer
//! intake; this module owns the *close* — flipping a matter to `closed`
//! when the firm signs its closing letter. Archival (the Drive cold
//! store) is a separate downstream step and is left untouched here.

use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::persons::Role;
use crate::postgres::PostgresDb;
pub use model::projects::is_valid_repository_url;
pub use model::projects::{
    drive_folder_id_from_input, is_valid_calendar_id, is_valid_code, is_valid_drive_folder_id,
    is_valid_resource_url, ClosureReason, ReopenReason, CALENDAR_ID_INVALID,
    DRIVE_FOLDER_ID_INVALID, DRIVE_FOLDER_INPUT_INVALID, RESOURCE_URL_INVALID,
};

/// Maximum length of a matter code, chosen to stay comfortably within
/// common filesystem and URL segment limits once `.git` is appended.
///
/// The same cap a Project **application** name carries, because they are the
/// same shape — see [`is_valid_code`].
pub const PROJECT_CODE_MAX_LEN: usize = cloud::workspace::SLUG_MAX_LEN;
const SLACK_CHANNEL_ID_INVALID: &str = "Slack channel id is invalid.";
const SLACK_CHANNEL_URL_INVALID: &str = "Slack channel URL is invalid.";
const JURISDICTION_UNKNOWN: &str = "That jurisdiction does not exist.";
const NOTION_PAGE_URL_INVALID: &str = "Notion page URL is invalid.";

/// A Project read from the SurrealDB projects cluster.
///
/// Every reference is a native link now: `entity_id` became a
/// `record<entity>` when the entities cluster ported (ENG-120), which was
/// the last cross-engine id this cluster carried.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub status: String,
    /// Which house brand's storefront this matter was opened through — a
    /// closed key from `views::brand::BrandKey` (`"neon"`,
    /// `"delete-your-data"`), written by the server from the resolved
    /// request host and never accepted from a client-submitted form. `store`
    /// does not depend on `views`, so this is a plain validated `String`
    /// rather than the enum itself; the SurrealDB schema's `ASSERT` is the
    /// single source of truth for which values are valid.
    pub brand: String,
    pub entity_id: Uuid,
    /// Owning practice. `None` until the row is pointed at a
    /// [`crate::firms::Firm`].
    pub firm_id: Option<Uuid>,
    /// The matter's governing jurisdiction — a general fact about the
    /// Project, read by whatever needs it. The pooled IOLTA account this
    /// matter's client funds sit in is derived from it
    /// ([`crate::iolta_accounts::for_project`]) rather than recorded a second
    /// time. `None` while the jurisdiction is unsettled, and never guessed
    /// from the client entity or the owning firm.
    pub jurisdiction_id: Option<Uuid>,
    /// The jurisdiction an onboarding Notation's Clauses bind to: the state
    /// whose law and arbitration seat an engagement letter names. Distinct
    /// from [`Self::jurisdiction_id`] — that one governs whose law the
    /// *matter* answers to; this one is the Firm's chosen drafting default
    /// for onboarding prose, deliberately editable, and read live at render
    /// time by a Clause input bound to `project.onboarding_jurisdiction`.
    pub onboarding_jurisdiction: Option<String>,
    /// The matter's client DRI — the Person a `CLIENT_DRI` special question
    /// and a `project.client_dri` Clause binding resolve to at render time
    /// (LAW-177). The participation model carries the accountability marker
    /// (`person_project_role.is_client_dri`); this column pins *which* of the
    /// client-side rows renders, so a template is never answered from a guess.
    /// Optional: a row written before the field existed has none, and nothing
    /// infers one. Every write goes through [`set_client_dri_person_id`],
    /// which refuses a Person holding no `is_client_dri` row on this matter —
    /// fails closed, never guessed.
    pub client_dri_person_id: Option<Uuid>,
    /// The legal-plan panel economics this matter carries (ENG-1269). All
    /// nine are optional: commercial-litigation and pro-bono matters leave
    /// every one unset, which is also how reporting separates the Firm's
    /// three matter lines.
    ///
    /// [`Self::legal_plan_id`] is a bare `uuid` pointer to the Firm's
    /// [`crate::legal_plans::LegalPlan`] row — that table is authored in
    /// PostgreSQL, so the foreign key lives there and the SurrealDB source
    /// carries the same UUID as a plain value.
    pub legal_plan_id: Option<Uuid>,
    /// The plan's member identifier for this matter, verbatim from the
    /// assignment.
    pub plan_member_id: Option<String>,
    /// The [Plan Case Number](../../docs/glossary/plan-case-number.md): the
    /// number the Legal Plan gave this referral (ARAG's CaseAssist number,
    /// LegalEASE's member case number), verbatim. Every plan email and
    /// claim cites it, so it is what links the matter back to how it is paid.
    pub plan_case_number: Option<String>,
    /// The benefit code the plan's assignment named, verbatim.
    pub benefit_code: Option<String>,
    /// Which kind of plan matter this is — a closed vocabulary shared with
    /// `plan_fee_schedule.matter_type` and the assumption keys, enforced by
    /// the PostgreSQL CHECK and [`crate::legal_plans::PlanMatterType`] at
    /// the write site. A plain validated `String` here, like [`Self::brand`],
    /// so a legacy row's value always reads back.
    pub plan_matter_type: Option<String>,
    /// The two-letter state code the plan matter answers to (`ny`, `nv`,
    /// `ca`, `wa`), lowercase, like `plan_fee_schedule.state`.
    pub plan_state: Option<String>,
    /// When the plan's assignment email landed (ISO `YYYY-MM-DD`). The
    /// acceptance clock starts here.
    pub plan_assigned_on: Option<String>,
    /// When the attorney accepted the staged matter (ISO `YYYY-MM-DD`).
    /// The attestation click and the turnaround clock start together.
    pub plan_accepted_on: Option<String>,
    /// When the matter closed (ISO `YYYY-MM-DD`), for plan turnaround
    /// reporting.
    pub plan_closed_on: Option<String>,
    /// Which kit version this matter applied (ENG-1271), recorded when a
    /// kit is applied so the digest's template-effect query can compare
    /// cohorts.
    pub kit_version: Option<String>,
    pub description: Option<String>,
    pub drive_folder_id: Option<String>,
    /// The full URL of the one source repository holding this Project's
    /// notation templates and its Portal.
    ///
    /// A whole URL rather than a name composed onto one deployment-wide forge
    /// host: a Project's source may live on any forge, in any organization, so
    /// GitHub, GitLab, and a self-hosted remote are all just values here.
    /// `None` means no repository is recorded, and nothing derives one from
    /// [`Self::code`] — a guessed URL would point at a namespace the Firm may
    /// not control.
    pub repository_url: Option<String>,
    pub git_initialized_at: Option<String>,
    pub forge_provisioned_at: Option<String>,
    pub closed_at: Option<String>,
    /// Why this matter was closed. `None` is retained for rows closed before
    /// closure reasons were recorded, rather than guessing a reason from
    /// later lifecycle evidence.
    pub closure_reason: Option<String>,
    /// The lawyer-only Slack channel for this matter, shown as a button on the
    /// lawyer workbench. Distinct from [`Self::external_slack_channel_url`]
    /// because a channel shared with the client carries different posting
    /// norms — conflating the two would risk lawyer-only chatter landing in a
    /// client-visible channel by mistake.
    pub internal_slack_channel_url: Option<String>,
    /// The Slack channel shared with the client, if this matter has one.
    /// Optional: most matters never get a client-facing channel.
    pub external_slack_channel_url: Option<String>,
    /// The Slack Web API channel ID for the private firm-side channel. This is
    /// distinct from the optional URL: the bot posts by ID, while a URL is a
    /// human-facing resource link that may not exist for every deployment.
    pub internal_slack_channel_id: Option<String>,
    /// The firm-only Notion page for this matter — the internal write-up,
    /// research, and working notes. Paired with
    /// [`Self::shared_notion_page_url`] for the same reason the two Slack
    /// columns are separate: one page is firm-only work product and the other
    /// is client-visible, and a single column would make a mistaken paste the
    /// difference between them.
    ///
    /// Navigator stores the address only. Who may open the page is governed in
    /// Notion's own sharing, which Navigator neither reads nor enforces — so a
    /// firm-only page must be shared to the firm's Notion group, not left on
    /// its workspace default.
    pub private_notion_page_url: Option<String>,
    /// The Notion page shared with the client, if this matter has one. Optional
    /// in exactly the way [`Self::external_slack_channel_url`] is.
    pub shared_notion_page_url: Option<String>,
    /// The matter's shared Google Calendar, by its Calendar API id. Firm-side:
    /// the calendar is shared with the firm-side participants only.
    #[serde(default)]
    pub calendar_id: Option<String>,
    /// Explicit IANA time zone for this Project. A blank update restores the derived default.
    #[serde(default)]
    pub calendar_timezone: Option<String>,
    /// Where the client books time: an appointment schedule a lawyer made on
    /// the Project calendar in Google Calendar, entered by hand because the
    /// Calendar API cannot create one. Shown to the client; cleared on close.
    #[serde(default)]
    pub booking_url: Option<String>,
    pub inserted_at: String,
    pub updated_at: String,
}

mod timezone;
pub use timezone::calendar_timezone;

/// Errors from the Project and participation seams.
#[derive(Debug, thiserror::Error)]
pub enum ProjectStoreError {
    #[error("PostgreSQL Project lifecycle state is unavailable")]
    LifecycleUnavailable,
    #[error(transparent)]
    Person(#[from] crate::persons::PersonError),
    #[error(transparent)]
    Firm(#[from] crate::firms::FirmError),
    #[error(transparent)]
    ExternalIdentity(Box<crate::external_identities::ExternalIdentityError>),
    #[error("that project code is already in use")]
    CodeTaken,
    /// A direct write tried to change `code` on an existing row. The engine
    /// refuses this itself (`project.code` is `READONLY`); this variant only
    /// gives the refusal a message that points at the rule.
    #[error(
        "a Project code cannot be changed after it is chosen at matter-open — see \
         docs/glossary/project.md"
    )]
    CodeImmutable,
    #[error("writing a project returned no usable row")]
    WriteReturnedNothing,
    #[error("no person {0}")]
    NoSuchPerson(Uuid),
    #[error("no project {0}")]
    NoSuchProject(Uuid),
    #[error("no firm {0}")]
    NoSuchFirm(Uuid),
    /// `NewProject.jurisdiction_id` names no `jurisdiction` row. Validated
    /// like `firm_id`: a matter's governing jurisdiction is a link readers
    /// dereference, so a dangling one is refused at the write.
    #[error("no jurisdiction {0}")]
    NoSuchJurisdiction(Uuid),
    #[error(transparent)]
    Jurisdictions(#[from] crate::jurisdictions::JurisdictionError),
    #[error("Project calendar time zone is invalid")]
    InvalidCalendarTimezone,
    #[error(transparent)]
    CalendarVenue(#[from] crate::cases::CaseError),
    /// `NewProject.brand` does not match any live `brand` row (ENG-587) —
    /// the same check `store::firms::attach_brand` runs, not a compiled
    /// closed list.
    #[error("unknown brand key {0}")]
    UnknownBrand(String),
    /// [`matter_lifecycle_sets`]'s batched onboarding/offboarding-artifact
    /// read failed. Kept distinct from [`ProjectStoreError::Db`] because that
    /// read spans four tables, not one.
    #[error("matter lifecycle: {0}")]
    Lifecycle(String),
    /// The person already holds a participation on the matter
    /// (`person_project_role_pair`).
    #[error("that person already participates in the matter")]
    DuplicateParticipation,
    #[error("no entity {0}")]
    NoSuchEntity(Uuid),
    #[error(transparent)]
    Tenancy(#[from] crate::postgres::tenancy::TenancyError),
    #[error(transparent)]
    Brand(Box<crate::brands::BrandError>),
    /// Under tenancy authority, PostgreSQL did not accept a Project write that
    #[error("PostgreSQL Project operation failed: {0}")]
    Postgres(String),
}

fn pg_project_error(
    error: crate::postgres::projects::ProjectError,
    input: &NewProject,
) -> ProjectStoreError {
    use crate::postgres::projects::ProjectError as Pg;
    match error {
        Pg::CodeTaken => ProjectStoreError::CodeTaken,
        Pg::CodeImmutable => ProjectStoreError::CodeImmutable,
        Pg::UnknownBrand => ProjectStoreError::UnknownBrand(input.brand.clone()),
        Pg::NoSuchEntity => ProjectStoreError::NoSuchEntity(input.entity_id),
        Pg::NoSuchFirm => ProjectStoreError::NoSuchFirm(input.firm_id.unwrap_or_default()),
        Pg::NoSuchJurisdiction => {
            ProjectStoreError::NoSuchJurisdiction(input.jurisdiction_id.unwrap_or_default())
        }
        Pg::WriteReturnedNothing => ProjectStoreError::WriteReturnedNothing,
        other => ProjectStoreError::Postgres(other.to_string()),
    }
}

impl From<crate::postgres::projects::ProjectError> for ProjectStoreError {
    fn from(error: crate::postgres::projects::ProjectError) -> Self {
        Self::Postgres(error.to_string())
    }
}

fn pg_participation_error(
    error: crate::postgres::participation::ParticipationError,
    project_id: Uuid,
    person_id: Uuid,
) -> ProjectStoreError {
    use crate::postgres::participation::ParticipationError as Pg;
    match error {
        Pg::NoSuchPerson => ProjectStoreError::NoSuchPerson(person_id),
        Pg::NoSuchProject => ProjectStoreError::NoSuchProject(project_id),
        Pg::Duplicate => ProjectStoreError::DuplicateParticipation,
        other => ProjectStoreError::Postgres(other.to_string()),
    }
}

impl From<ProjectStoreError> for ProjectCommandError {
    fn from(error: ProjectStoreError) -> Self {
        Self::Db(error.to_string())
    }
}

/// The admitted PostgreSQL handle, or `None` only inside explicit Source import reads.
async fn authority() -> Result<PostgresDb, ProjectStoreError> {
    Ok(Box::pin(crate::postgres::tenancy::db()).await?)
}

impl From<crate::external_identities::ExternalIdentityError> for ProjectStoreError {
    fn from(error: crate::external_identities::ExternalIdentityError) -> Self {
        Self::ExternalIdentity(Box::new(error))
    }
}

/// Inputs for creating a Project row. Command callers validate the
/// `entity_id` before calling this function: the engine does not validate
/// a `record<>` link, so a dangling one is accepted here and caught above.
#[derive(Debug, Clone)]
pub struct NewProject {
    pub code: String,
    pub name: String,
    pub status: String,
    /// Which house brand's storefront this matter was opened through — see
    /// [`Project::brand`]. Defaults to `"neon"` (below) rather than the
    /// derived empty string, because the schema's `ASSERT` rejects an empty
    /// value: the many internal and fixture callers that build a `NewProject`
    /// with `..Default::default()` never touch this field, and `"neon"` is
    /// the correct value for every one of them. A real client-intake open
    /// (`portal::retainer_walk`) sets it explicitly from the resolved request
    /// host instead.
    pub brand: String,
    pub entity_id: Uuid,
    /// Owning practice. `None` on a row that has not yet been pointed at a
    /// firm — every existing caller that uses [`Default`] keeps that shape.
    pub firm_id: Option<Uuid>,
    /// The matter's governing jurisdiction — see [`Project::jurisdiction_id`].
    /// `None` by default: a matter opened before its jurisdiction is settled
    /// carries none, and [`set_jurisdiction`] fills it later.
    pub jurisdiction_id: Option<Uuid>,
    pub description: Option<String>,
}

impl Default for NewProject {
    fn default() -> Self {
        Self {
            code: String::new(),
            name: String::new(),
            status: String::new(),
            brand: "neon".to_string(),
            entity_id: Uuid::nil(),
            firm_id: None,
            jurisdiction_id: None,
            description: None,
        }
    }
}

/// One person's current participation on a Project.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PersonProjectRole {
    pub id: Uuid,
    pub person_id: Uuid,
    pub project_id: Uuid,
    pub participation: String,
    pub is_lawyer_dri: bool,
    pub is_client_dri: bool,
    pub inserted_at: String,
    pub updated_at: String,
}

/// The participation kinds on the client side of a matter. `counterparty`
/// remains listed because a legacy row must keep reading client-side: promoting
/// an adverse party to the firm lens is the one direction that must never
/// happen by omission. Nothing writes it any more.
pub const PARTICIPATION_CLIENT_SIDE: &[&str] = &["client", "counterparty"];

/// The matter-side participation implied by a person's system tier — the only
/// way a participation is ever chosen.
///
/// Which side of a matter someone is on follows from what they are: a `client`
/// lands on the client side, and every firm tier lands firm-side under its own
/// name. There is no second vocabulary to disagree with the tier. The kinds that
/// used to need one are not participants at all — an adverse party never gets a
/// portal row, and co-counsel working the matter is a `lawyer` person.
#[must_use]
pub fn participation_for_role(role: Role) -> &'static str {
    role.as_str()
}

/// All participation rows for a matter, in stable insertion order.
pub async fn participations_for_project(
    _application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<PersonProjectRole>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::for_project_on(
        &pg, project_id,
    ))
    .await
    .map_err(|error| pg_participation_error(error, project_id, Uuid::nil()))
}

/// People currently designated lawyer DRI on a matter, in insertion order.
pub async fn lawyer_dri_people(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<crate::persons::Person>, ProjectStoreError> {
    let mut people = Vec::new();
    for row in participations_for_project(application_store, project_id)
        .await?
        .into_iter()
        .filter(|row| row.is_lawyer_dri)
    {
        if let Some(person) = crate::persons::find_by_id(application_store, row.person_id).await? {
            people.push(person);
        }
    }
    Ok(people)
}

/// Every participation row, ordered for the lawyer directory.
pub async fn all_participations(
    _application_store: &ApplicationStore,
) -> Result<Vec<PersonProjectRole>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::all_on(&pg))
        .await
        .map_err(|error| pg_participation_error(error, Uuid::nil(), Uuid::nil()))
}

/// One participation row by its id.
pub async fn participation_by_id(
    _application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<PersonProjectRole>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::by_id_on(&pg, id))
        .await
        .map_err(|error| pg_participation_error(error, Uuid::nil(), Uuid::nil()))
}

/// Write `columns` to the PostgreSQL Project first, then copy the committed row
/// into the SurrealDB compatibility copy. `None` when no Project has that id.
async fn patch_in_postgres(
    _application_store: &ApplicationStore,
    pg: &PostgresDb,
    project_id: Uuid,
    columns: &[crate::postgres::projects::Column<'_>],
    within_firm: Option<(Uuid, Option<&str>)>,
) -> Result<Option<Project>, ProjectStoreError> {
    let Some(project) = Box::pin(crate::postgres::projects::update_on(
        pg,
        project_id,
        columns,
        within_firm,
    ))
    .await?
    else {
        return Ok(None);
    };

    Ok(Some(project))
}

/// Patch the Project on the admitted PostgreSQL application pool.
async fn patch_under_authority(
    application_store: &ApplicationStore,
    project_id: Uuid,
    columns: &[crate::postgres::projects::Column<'_>],
) -> Result<Option<Project>, ProjectCommandError> {
    let pg = authority().await?;
    patch_in_postgres(application_store, &pg, project_id, columns, None)
        .await
        .map_err(Into::into)
}

/// Add one person to a Project, validating both record links at the command
/// seam. The unique pair index is the concurrency backstop.
pub async fn add_participation(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
    _participation: &str,
) -> Result<PersonProjectRole, ProjectStoreError> {
    let pg = authority().await?;
    // PostgreSQL derives the label from the Person's tier
    // ([`participation_for_role`]) inside the insert's transaction.
    let role = Box::pin(crate::postgres::participation::add_on(
        &pg, project_id, person_id,
    ))
    .await
    .map_err(|error| pg_participation_error(error, project_id, person_id))?;

    Ok(role)
}

/// Replace a participation's person and label without disturbing its DRI
/// markers. The caller enforces the DRI-side rule before invoking this write.
pub async fn update_participation(
    _application_store: &ApplicationStore,
    role_id: Uuid,
    person_id: Uuid,
    _participation: &str,
) -> Result<Option<PersonProjectRole>, ProjectStoreError> {
    let pg = authority().await?;
    let role = Box::pin(crate::postgres::participation::update_on(
        &pg, role_id, person_id,
    ))
    .await
    .map_err(|error| pg_participation_error(error, Uuid::nil(), person_id))?;
    Ok(role)
}

/// Remove one participation row. The caller checks the lawyer-DRI invariant
/// before deletion; this low-level command is intentionally idempotent.
pub async fn remove_participation(
    _application_store: &ApplicationStore,
    role_id: Uuid,
) -> Result<(), ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::remove_on(&pg, role_id))
        .await
        .map_err(|error| pg_participation_error(error, Uuid::nil(), Uuid::nil()))
}

/// Read the participation that gives `person_id` scope on `project_id`.
pub async fn participation_for_person(
    _application_store: &ApplicationStore,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<Option<PersonProjectRole>, ProjectStoreError> {
    let pg = PostgresDb::application()
        .await
        .map_err(|e| ProjectStoreError::Postgres(e.to_string()))?;
    Box::pin(crate::postgres::participation::for_person_on(
        &pg, person_id, project_id,
    ))
    .await
    .map_err(|error| pg_participation_error(error, project_id, person_id))
}

/// All participation rows held by one person, in stable insertion order.
pub async fn participations_for_person(
    _application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<Vec<PersonProjectRole>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::for_person_projects_on(
        &pg, person_id,
    ))
    .await
    .map_err(|error| pg_participation_error(error, Uuid::nil(), person_id))
}

/// Whether this person holds a firm-side participation row on the matter.
///
/// The membership question on its own, with no tier policy folded in:
/// client-side participations and client DRIs never enter the lawyer lens,
/// while unrecognized participation strings remain firm-side. Callers layer
/// their own tier rule over it — `store::access` requires the row of every
/// tier, while [`can_access_as_lawyer`] keeps the Owner/Admin
/// short-circuit its own callers still depend on.
pub async fn has_firm_participation(
    application_store: &ApplicationStore,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<bool, ProjectStoreError> {
    Ok(
        participation_for_person(application_store, person_id, project_id)
            .await?
            .is_some_and(|row| {
                !PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
                    && !row.is_client_dri
            }),
    )
}

/// Whether an Owner or Firm Admin may oversee this Project without participation.
pub async fn can_administer_project(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project: &Project,
) -> Result<bool, ProjectStoreError> {
    if role != Role::Admin {
        return Ok(role.is_owner());
    }
    let firm_id = match project.firm_id {
        Some(firm_id) => Some(firm_id),
        None => crate::firms::firm_id_for_brand_key(application_store, &project.brand).await?,
    };
    let Some(firm_id) = firm_id else {
        return Ok(true);
    };
    let decision = crate::firm_capability::FirmCapability::ManageMembership
        .resolve(application_store, role, person_id, firm_id)
        .await?;
    Ok(decision.is_allowed())
}

/// Legacy firm-side command access follows Project participation and allows
/// oversight by the Owner or an Admin of the owning Firm.
pub async fn can_access_as_lawyer(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<bool, ProjectStoreError> {
    if !role.is_lawyer_tier() {
        return Ok(false);
    }
    if role.is_owner() {
        return Ok(true);
    }
    if role == Role::Admin {
        let Some(project) = find_by_id(application_store, project_id).await? else {
            return Ok(false);
        };
        if can_administer_project(application_store, person_id, role, &project).await? {
            return Ok(true);
        }
    }
    let Some(person_id) = person_id else {
        return Ok(false);
    };
    has_firm_participation(application_store, person_id, project_id).await
}

/// Whether a person may read a Project through the client lens.
///
/// The projects cluster is authoritative for both the project row and its
/// participation rows. The client-side predicate lives here beside the
/// lawyer predicate so that every access decision reads one membership
/// table.
pub async fn can_access_as_client(
    application_store: &ApplicationStore,
    person_id: Option<Uuid>,
    project_id: Uuid,
) -> Result<bool, ProjectStoreError> {
    let Some(person_id) = person_id else {
        return Ok(false);
    };
    Ok(
        participation_for_person(application_store, person_id, project_id)
            .await?
            .is_some_and(|row| {
                PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str()) || row.is_client_dri
            }),
    )
}

/// One matter as the Owner/Admin directory lens sees it: what the matter is,
/// and who is accountable for it.
///
/// Deliberately four fields. The lens answers "which matters exist, and who
/// owns each" and nothing further, so there is no project id here to hang a
/// detail link on: [`code`](Self::code) is the stable handle (`project_code`
/// is `UNIQUE`), and anything a matter *contains* — notations, deadlines,
/// documents, communications, the rest of the participation ledger — is
/// membership's to disclose, not oversight's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MatterDirectoryEntry {
    pub code: String,
    pub name: String,
    pub status: String,
    /// The brand key the matter's portal wears (ENG-587: always present,
    /// never a compiled closed list).
    pub brand: String,
    /// The names on the matter's `is_lawyer_dri` rows, alphabetical, or empty
    /// when no lawyer holds the marker. An unassigned matter is what this lens
    /// exists to surface, so an empty set is an ordinary value here rather than
    /// an error.
    pub lawyer_dris: Vec<String>,
}

/// The Owner/Admin directory over every matter.
///
/// The third shape beside the two predicates above, and neither of them:
/// [`can_access_as_lawyer`] short-circuits the admin tier to full
/// access to a matter, and `store::access` requires a participation row of
/// every tier and denies without one. This one is oversight rather than
/// membership — see that a matter exists and who owns it, without seeing what
/// is in it — so it returns the projection instead of a bool, and Owner and
/// Admin reach it holding no `person_project_roles` row at all.
///
/// Admin tier only. A `lawyer` caller gets an empty directory, the same
/// answer they get from every other firm-wide read they are not entitled to;
/// the tier is checked before the read runs, so a `lawyer` call touches
/// nothing.
pub async fn matter_directory(
    application_store: &ApplicationStore,
    role: Role,
) -> Result<Vec<MatterDirectoryEntry>, ProjectStoreError> {
    if !role.is_admin_tier() {
        return Ok(Vec::new());
    }
    let projects = all(application_store).await?;
    if projects.is_empty() {
        return Ok(Vec::new());
    }

    let dris_by_project = dri_names_by_project(application_store, DriSide::Lawyer).await?;
    Ok(projects
        .into_iter()
        .map(|project| MatterDirectoryEntry {
            // A matter with no flagged row reads as unassigned rather than
            // an error.
            lawyer_dris: dris_by_project
                .get(&project.id)
                .cloned()
                .unwrap_or_default(),
            code: project.code,
            name: project.name,
            status: project.status,
            brand: project.brand,
        })
        .collect())
}

/// Owner sees every matter. Admin sees only matters owned by a firm they
/// belong to. An Admin with no `person_firm_role` row gets an empty
/// directory rather than the deployment-wide listing.
///
/// Routes through [`crate::firm_capability::allowed_firm_ids`] with
/// [`crate::firm_capability::FirmCapability::ViewDirectory`] (ENG-463) rather
/// than deriving its own membership set, so this stays in step with every
/// other Firm-scoped directory read.
pub async fn matter_directory_for(
    application_store: &ApplicationStore,
    role: Role,
    viewer_person_id: Option<Uuid>,
) -> Result<Vec<MatterDirectoryEntry>, ProjectStoreError> {
    let entries = matter_directory(application_store, role).await?;
    if role == Role::Owner {
        return Ok(entries);
    }
    if role != Role::Admin {
        return Ok(entries);
    }
    let Some(person_id) = viewer_person_id else {
        return Ok(Vec::new());
    };
    let firm_ids = crate::firm_capability::allowed_firm_ids(
        application_store,
        Role::Admin,
        Some(person_id),
        crate::firm_capability::FirmCapability::ViewDirectory,
    )
    .await?;
    if firm_ids.is_empty() {
        return Ok(Vec::new());
    }
    let owned: std::collections::BTreeSet<_> = all(application_store)
        .await?
        .into_iter()
        .filter(|project| {
            project
                .firm_id
                .is_some_and(|firm_id| firm_ids.contains(&firm_id))
        })
        .map(|project| project.code)
        .collect();
    Ok(entries
        .into_iter()
        .filter(|entry| owned.contains(&entry.code))
        .collect())
}

/// Every project's names on one DRI side, keyed by project id, for every
/// project that has at least one. One round trip per table, not one per
/// matter: every flagged row, then the persons they name — the same batching
/// `matter_lifecycle_sets` uses.
///
/// A flagged row naming a person who is no longer there drops out rather than
/// becoming a name the caller cannot produce. The set has no inherent order,
/// so each project's names come back sorted rather than left in row-scan
/// order.
pub(crate) async fn dri_names_by_project(
    application_store: &ApplicationStore,
    side: DriSide,
) -> Result<std::collections::HashMap<Uuid, Vec<String>>, ProjectStoreError> {
    let mut ids_by_project: std::collections::HashMap<Uuid, Vec<Uuid>> =
        std::collections::HashMap::new();
    for row in all_participations(application_store)
        .await?
        .into_iter()
        .filter(|row| match side {
            DriSide::Lawyer => row.is_lawyer_dri,
            DriSide::Client => row.is_client_dri,
        })
    {
        ids_by_project
            .entry(row.project_id)
            .or_default()
            .push(row.person_id);
    }
    let dri_ids: Vec<Uuid> = ids_by_project.values().flatten().copied().collect();
    let names: std::collections::HashMap<Uuid, String> =
        crate::persons::find_by_ids(application_store, &dri_ids)
            .await?
            .into_iter()
            .map(|person| (person.id, person.name))
            .collect();
    Ok(ids_by_project
        .into_iter()
        .map(|(project_id, ids)| {
            let mut names: Vec<String> = ids
                .into_iter()
                .filter_map(|id| names.get(&id).cloned())
                .collect();
            names.sort();
            (project_id, names)
        })
        .collect())
}

/// One project's accountable people on both DRI sides — the shape the nightly
/// `DriDigest` Slack notice renders one line per project from.
///
/// `Deserialize` too (unlike [`MatterDirectoryEntry`]): this is the journaled
/// output of the workflow's `query` step, and Restate must be able to replay
/// it back out of the journal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectDriSummary {
    pub code: String,
    pub name: String,
    pub status: String,
    /// The names on the project's `is_lawyer_dri` rows, alphabetical, or
    /// empty when no lawyer holds the marker.
    pub lawyer_dris: Vec<String>,
    /// The names on the project's `is_client_dri` rows, alphabetical, or
    /// empty when no client holds the marker.
    pub client_dris: Vec<String>,
}

/// An internal Project Slack channel and its eligible Firm-side participants.
/// Email fallbacks and recorded Slack identities stay in the sync operation;
/// callers must not log or journal these addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSlackMembershipTarget {
    pub project_id: Uuid,
    pub code: String,
    pub status: String,
    /// `None` until a channel is created or adopted for the Project.
    pub channel_id: Option<String>,
    pub member_emails: Vec<String>,
    /// Recorded provider addresses, resolved only after role and Firm scope.
    pub member_slack_ids: Vec<String>,
}

/// Every Project's Slack target, and how many Projects the sync considered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSlackMembershipTargets {
    pub projects: usize,
    pub targets: Vec<ProjectSlackMembershipTarget>,
}

/// Every Project and the Firm-side participants eligible for its internal Slack
/// channel. Recorded Slack identities replace email lookup. A Project without
/// a channel is still a target, so the sync can create or adopt one.
pub async fn slack_membership_targets(
    application_store: &ApplicationStore,
) -> Result<ProjectSlackMembershipTargets, ProjectStoreError> {
    let projects = all(application_store).await?;
    let project_count = projects.len();
    let mut targets = Vec::new();
    for project in projects {
        targets.push(membership_target_for(application_store, project).await?);
    }
    Ok(ProjectSlackMembershipTargets {
        projects: project_count,
        targets,
    })
}

/// One Project's Slack target, by the same rules as [`slack_membership_targets`].
/// `None` when the Project no longer exists.
pub async fn slack_membership_target(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<ProjectSlackMembershipTarget>, ProjectStoreError> {
    match find_by_id(application_store, project_id).await? {
        Some(project) => Ok(Some(
            membership_target_for(application_store, project).await?,
        )),
        None => Ok(None),
    }
}

async fn membership_target_for(
    application_store: &ApplicationStore,
    project: Project,
) -> Result<ProjectSlackMembershipTarget, ProjectStoreError> {
    let participations = participations_for_project(application_store, project.id).await?;
    let mut emails = std::collections::BTreeSet::new();
    let mut slack_ids = std::collections::BTreeSet::new();
    for row in participations
        .into_iter()
        .filter(crate::access::is_firm_participation)
    {
        let Some(person) = crate::persons::find_by_id(application_store, row.person_id).await?
        else {
            continue;
        };
        if person.role == crate::persons::Role::Client {
            continue;
        }
        if let Some(identity) = crate::external_identities::find_by_person_and_system(
            application_store,
            person.id,
            crate::external_identities::ExternalSystem::Slack,
        )
        .await?
        {
            slack_ids.insert(identity.external_id);
        } else {
            emails.insert(person.email.to_ascii_lowercase());
        }
    }
    Ok(ProjectSlackMembershipTarget {
        project_id: project.id,
        code: project.code,
        status: project.status,
        channel_id: project.internal_slack_channel_id,
        member_emails: emails.into_iter().collect(),
        member_slack_ids: slack_ids.into_iter().collect(),
    })
}

/// A Lawyer DRI as the internal channel canvas names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCanvasDri {
    pub name: String,
    pub email: String,
}

/// What the internal Slack channel canvas says about one Project. Directory
/// emails are on it for the **internal** channel only; callers must not log or
/// journal them. The customer Slack channel is not a field here, so nothing
/// built from this can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCanvas {
    pub code: String,
    pub name: String,
    pub status: String,
    pub description: Option<String>,
    pub repository_url: Option<String>,
    /// `None` until a channel is created or adopted for the Project.
    pub internal_channel_id: Option<String>,
    /// Client DRI directory emails, sorted.
    pub client_dri_emails: Vec<String>,
    /// Lawyer DRIs, sorted by name then email.
    pub lawyer_dris: Vec<ProjectCanvasDri>,
}

/// One Project's canvas projection. `None` when the Project no longer exists.
/// Unlike [`slack_membership_target`], it reads both DRI sides, and it never
/// feeds an invite list: a Client DRI appears here without joining a channel.
pub async fn project_canvas(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<ProjectCanvas>, ProjectStoreError> {
    let Some(project) = find_by_id(application_store, project_id).await? else {
        return Ok(None);
    };
    let mut client_dri_emails = Vec::new();
    let mut lawyer_dris = Vec::new();
    for row in participations_for_project(application_store, project.id).await? {
        if !row.is_client_dri && !row.is_lawyer_dri {
            continue;
        }
        let Some(person) = crate::persons::find_by_id(application_store, row.person_id).await?
        else {
            continue;
        };
        if row.is_client_dri {
            client_dri_emails.push(person.email.clone());
        }
        if row.is_lawyer_dri {
            lawyer_dris.push(ProjectCanvasDri {
                name: person.name,
                email: person.email,
            });
        }
    }
    client_dri_emails.sort();
    client_dri_emails.dedup();
    lawyer_dris.sort_by(|a, b| (&a.name, &a.email).cmp(&(&b.name, &b.email)));
    lawyer_dris.dedup();
    Ok(Some(ProjectCanvas {
        code: project.code,
        name: project.name,
        status: project.status,
        description: project.description,
        repository_url: project.repository_url,
        internal_channel_id: project.internal_slack_channel_id,
        client_dri_emails,
        lawyer_dris,
    }))
}

/// Each open project's DRIs on both sides, for the nightly digest.
///
/// Unlike [`matter_directory`], this has no role gate: the caller is a
/// headless workflow with no session, not a person, and the digest's whole
/// purpose is to surface a side left unassigned — on either side — so both
/// are read here, where `matter_directory` deliberately reads only the
/// lawyer side for its Owner/Admin oversight view.
pub async fn dri_digest(
    application_store: &ApplicationStore,
) -> Result<Vec<ProjectDriSummary>, ProjectStoreError> {
    let mut projects = all(application_store).await?;
    projects.retain(|project| project.status == "open");
    if projects.is_empty() {
        return Ok(Vec::new());
    }
    let lawyer_by_project = dri_names_by_project(application_store, DriSide::Lawyer).await?;
    let client_by_project = dri_names_by_project(application_store, DriSide::Client).await?;
    Ok(projects
        .into_iter()
        .map(|project| ProjectDriSummary {
            lawyer_dris: lawyer_by_project
                .get(&project.id)
                .cloned()
                .unwrap_or_default(),
            client_dris: client_by_project
                .get(&project.id)
                .cloned()
                .unwrap_or_default(),
            code: project.code,
            name: project.name,
            status: project.status,
        })
        .collect())
}

/// Add a person to a Project's lawyer or client DRI set.
///
/// Designation is **additive**: a matter carries as many accountable people per
/// side as the firm has put there, so taking the marker displaces nobody. Giving
/// it up is [`clear_dri`], and that is the only way a person leaves a
/// set.
///
/// The transaction writes the Project itself first: concurrent DRI changes to
/// the same Project conflict on that row and retry through the typed conflict
/// loop, so the set-emptiness rule `store::participation` checks beforehand
/// cannot be raced by two removals that each saw a second holder.
pub async fn designate_dri(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    person_id: Uuid,
    side: DriSide,
) -> Result<(), ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::designate_dri_on(
        &pg, project_id, person_id, side,
    ))
    .await
    .map_err(|error| pg_participation_error(error, project_id, person_id))?;
    Ok(())
}

/// Drop one participation row's DRI marker for `side`.
///
/// The inverse of [`designate_dri`], and the only way a person leaves
/// a DRI set. It takes one row out and leaves the rest of the side alone; that
/// the lawyer set never empties is `store::participation`'s rule, checked above
/// this call.
pub async fn clear_dri(
    _application_store: &ApplicationStore,
    role_id: Uuid,
    side: DriSide,
) -> Result<(), ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::participation::clear_dri_on(
        &pg, role_id, side,
    ))
    .await
    .map_err(|error| pg_participation_error(error, Uuid::nil(), Uuid::nil()))?;
    Ok(())
}

/// Create a Project under a fresh UUID record key.
///
/// Writes retry only typed Surreal transaction conflicts; a code collision is
/// a caller-correctable conflict and is never retried as if it were transient.
pub async fn create(
    _application_store: &ApplicationStore,
    input: &NewProject,
) -> Result<Project, ProjectStoreError> {
    let pg = authority().await?;
    let project = Box::pin(crate::postgres::projects::create_on(&pg, input))
        .await
        .map_err(|error| pg_project_error(error, input))?;

    Ok(project)
}

/// Write one archived Project row back under its original id and code, every
/// field as archived. An identical row is left alone and a different row with
/// the same id or code is a conflict; nothing is overwritten.
///
/// # Errors
/// [`ProjectStoreError`] on a database failure.
pub async fn restore_archived(
    application_store: &ApplicationStore,
    project: &Project,
) -> Result<crate::assets::RowRestore, ProjectStoreError> {
    if let Some(existing) = find_by_id(application_store, project.id).await? {
        return Ok(if &existing == project {
            crate::assets::RowRestore::Identical
        } else {
            crate::assets::RowRestore::Conflict
        });
    }
    if find_by_code(application_store, &project.code)
        .await?
        .is_some()
    {
        return Ok(crate::assets::RowRestore::Conflict);
    }
    let pg = authority().await?;
    Ok(crate::postgres::projects::restore_on(&pg, project).await?)
}

/// Whether a Project other than `project_id` records `drive_folder_id`. The
/// Drive importer refuses a folder another matter owns, so the folder link,
/// not its display name, is what scopes an import.
///
/// # Errors
/// [`ProjectStoreError`] on a database failure.
pub async fn drive_folder_recorded_elsewhere(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    drive_folder_id: &str,
) -> Result<bool, ProjectStoreError> {
    let pg = authority().await?;
    crate::postgres::projects::drive_folder_recorded_elsewhere_on(&pg, project_id, drive_folder_id)
        .await
        .map_err(ProjectStoreError::from)
}

/// Find the Project carrying `code`, whose UNIQUE index makes it at most one.
///
/// Fixtures resolve by code rather than id: the code is the stable name a seed
/// knows, while the id is whatever the engine that first created the record
/// assigned.
pub async fn find_by_code(
    _application_store: &ApplicationStore,
    code: &str,
) -> Result<Option<Project>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::projects::find_by_code_on(&pg, code))
        .await
        .map_err(ProjectStoreError::from)
}

/// Resolve the Project carrying `code`, creating it under `id` if absent.
///
/// Racing callers settle on one record rather than one of them failing: `code`
/// is UNIQUE, so the loser of a create race re-reads and adopts the winner's
/// record. The cucumber suites need this — they run scenarios concurrently
/// against one shared engine, each re-seeding the same fixture codes. Mirrors
/// [`crate::persons::find_or_create`].
pub async fn find_or_create_by_code(
    application_store: &ApplicationStore,
    id: Uuid,
    input: &NewProject,
) -> Result<Project, ProjectStoreError> {
    if let Some(existing) = find_by_code(application_store, &input.code).await? {
        return Ok(existing);
    }
    match upsert_with_id(application_store, id, input).await {
        Err(ProjectStoreError::CodeTaken) => find_by_code(application_store, &input.code)
            .await?
            .ok_or(ProjectStoreError::WriteReturnedNothing),
        other => other,
    }
}

/// Create or update the Project carrying `id`, rather than minting a new one.
///
/// [`create`] owns id generation, which is right for a real matter open. A
/// fixture that must exist in both engines under a single id cannot use it:
/// the caller already holds the id and needs this cluster to agree with it.
/// Idempotent, so re-running a seed over a persisted database reconciles the
/// record instead of duplicating it.
pub async fn upsert_with_id(
    _application_store: &ApplicationStore,
    id: Uuid,
    input: &NewProject,
) -> Result<Project, ProjectStoreError> {
    let pg = authority().await?;
    let project = Box::pin(crate::postgres::projects::upsert_with_id_on(&pg, id, input))
        .await
        .map_err(|error| pg_project_error(error, input))?;

    Ok(project)
}

/// Find the Project identified by `id` in the projects cluster.
///
/// This is the validation read a writer uses to prove a project id names a
/// real row before storing a reference to it.
pub async fn find_by_id(
    _application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Option<Project>, ProjectStoreError> {
    let pg = PostgresDb::application()
        .await
        .map_err(|e| ProjectStoreError::Postgres(e.to_string()))?;
    Box::pin(crate::postgres::projects::find_by_id_on(&pg, id))
        .await
        .map_err(ProjectStoreError::from)
}

/// Find a Project by its display name. Seeds use this only as their natural
/// key lookup; application commands address Projects by UUID.
pub async fn find_by_name(
    _application_store: &ApplicationStore,
    name: &str,
) -> Result<Option<Project>, ProjectStoreError> {
    let pg = authority().await?;
    Box::pin(crate::postgres::projects::find_by_name_on(&pg, name))
        .await
        .map_err(ProjectStoreError::from)
}

/// Every project in stable lawyer-list order.
pub async fn all(_application_store: &ApplicationStore) -> Result<Vec<Project>, ProjectStoreError> {
    let pg = PostgresDb::application()
        .await
        .map_err(|e| ProjectStoreError::Postgres(e.to_string()))?;
    Box::pin(crate::postgres::projects::all_on(&pg))
        .await
        .map_err(ProjectStoreError::from)
}

/// Which side of a matter a DRI designation applies to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DriSide {
    Lawyer,
    Client,
}

/// Normalize free text into the alphanumeric kebab-case base of a matter code.
/// The generated code appends a stable letter suffix for uniqueness.
///
/// Digits survive. A name carrying a numeral — a company name ending in one, a
/// statute or form number, a matter named for a section — keeps it, rather than
/// having it collapsed into a separator: the code is what the matter's route,
/// portal mount, and documents-bucket prefix use, so a numbered filing has to
/// be expressible as a code.
#[must_use]
pub fn code_base_from_name(name: &str) -> String {
    let mut out = String::new();
    let mut last_was_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_was_dash = false;
        } else if !out.is_empty() && !last_was_dash {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "project".to_string()
    } else {
        out
    }
}

fn letter_suffix(project_id: Uuid) -> String {
    let mut n = project_id.as_u128();
    let mut suffix = ['a'; 8];
    for ch in suffix.iter_mut().rev() {
        *ch = char::from(b'a' + u8::try_from(n % 26).expect("modulo 26 fits u8"));
        n /= 26;
    }
    suffix.into_iter().collect()
}

/// Generate a unique default matter code from its display name and UUID.
#[must_use]
pub fn code_from_name(name: &str, project_id: Uuid) -> String {
    let suffix = letter_suffix(project_id);
    let max_base = PROJECT_CODE_MAX_LEN - suffix.len() - 1;
    let mut base: String = code_base_from_name(name).chars().take(max_base).collect();
    while base.ends_with('-') {
        base.pop();
    }
    format!("{base}-{suffix}")
}

/// How many hex characters of the digest a [`random_code`] keeps: 64 bits,
/// so two self-serve intakes never meet on one code in practice.
const RANDOM_CODE_LEN: usize = 16;

/// A fresh, unguessable matter code for a Project opened by self-serve
/// intake: the leading hex of a SHA-256 over 32 random bytes. It carries
/// nothing about the client — the code is a URL segment, a repository name,
/// and a storage prefix, so a name- or email-derived code would publish it.
#[must_use]
pub fn random_code() -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let seed: [u8; 32] = rand::random();
    Sha256::digest(seed).iter().take(RANDOM_CODE_LEN / 2).fold(
        String::with_capacity(RANDOM_CODE_LEN),
        |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        },
    )
}

/// Normalize a manually supplied matter code. Returns `None` when the
/// input is blank.
#[must_use]
pub fn normalize_code(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_ascii_lowercase())
    }
}

/// Resolve a `{project_code}` segment to its Project's internal id.
///
/// `None` covers every miss the same way — a malformed code, a code no Project
/// carries, or a store error — because a caller must not be able to tell them
/// apart. Each handler turns that into its own non-disclosing refusal, which is
/// 404 everywhere below `/app`: a 403 would confirm to a stranger that a matter
/// with this code exists.
///
/// The code is validated before the store is asked. That refuses `new` — the
/// matter-open form rather than a matter — and keeps a malformed segment from
/// reaching a query at all.
///
/// A caller that owes its reader a distinct answer for a store failure calls
/// [`find_by_code`] directly and maps the error itself: an outage must read as
/// `500`, not as "your matter is gone".
pub async fn id_for_code(application_store: &ApplicationStore, project_code: &str) -> Option<Uuid> {
    if !is_valid_code(project_code) {
        return None;
    }
    find_by_code(application_store, project_code)
        .await
        .ok()
        .flatten()
        .map(|project| project.id)
}

/// Human-readable reason [`is_valid_repository_url`] refuses a value, used as
/// the caller-correctable message on the command boundary.
pub const REPOSITORY_URL_INVALID: &str =
    "A repository URL must be an http(s):// URL naming a host and a path.";

/// Human-readable reason a well-formed Drive folder id is refused because the
/// deployment's Drive provider does not find it as a live folder in the
/// Firm's Projects shared drive.
pub const DRIVE_FOLDER_NOT_IN_PROJECTS_DRIVE: &str =
    "That Drive folder is not a live folder in the Firm's Projects shared drive; move it there or share the shared drive with Navigator first.";

/// Human-readable reason [`drive_folder_recorded_elsewhere`] refuses a patch.
pub const DRIVE_FOLDER_RECORDED_ELSEWHERE: &str = "That Drive folder is already the ingest folder of another matter; a folder scopes its import to the one matter that records it.";

/// Whether a value has the shape of a recorded Kit version:
/// `{jurisdiction}/{practice_area}/{version}` — three non-empty path segments
/// with no whitespace. The value is displayed as recorded and matched against
/// the Project Skill catalog's `version` field, so anything beyond the
/// segment shape is accepted verbatim rather than second-guessed here.
#[must_use]
pub fn is_valid_kit_version(value: &str) -> bool {
    let segments: Vec<&str> = value.split('/').collect();
    segments.len() == 3
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && !segment.chars().any(char::is_whitespace))
}

/// The notation id of the person's **sole open matter**, for auto-routing an
/// inbound message to a matter without manual triage. Returns `Some` only
/// when the person is the client (`notations.person_id`) on exactly one
/// matter whose project is still `open`; `None` when they have none, or more
/// than one (the ambiguous case — fall back to manual `@link`).
///
/// This is the seam the email loop uses so a known client's reply lands on
/// their matter's conversation log on its own.
///
/// # Errors
///
/// Propagates any database error.
pub async fn sole_open_matter_for_person(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<Option<Uuid>, String> {
    let notations = crate::notations::list_by_person(application_store, person_id)
        .await
        .map_err(|error| error.to_string())?;

    let mut open: Vec<Uuid> = Vec::new();
    for n in notations {
        if let Some(p) = find_by_id(application_store, n.project_id)
            .await
            .map_err(|error| error.to_string())?
        {
            if p.status == "open" {
                open.push(n.id);
            }
        }
    }
    Ok((open.len() == 1).then(|| open[0]))
}

/// Flip the matter that `notation_id` belongs to from `open` to
/// `closed`. Returns the closed project's id, or `None` if the notation
/// (or its project) no longer exists.
///
/// Idempotent and monotonic: a matter already `closed` or `archived` is
/// left as-is — re-running never re-opens it, and a replay of the
/// firm-signature side effect is a no-op. `inserted_at`/`updated_at` are
/// maintained by the entity's active-model behavior.
pub async fn close_for_notation(
    application_store: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<Uuid>, String> {
    let Some(n) = crate::notations::find_by_id(application_store, notation_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let Some(p) = find_by_id(application_store, n.project_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let project_id = p.id;
    // Monotonic: don't walk backwards out of `archived`, and don't
    // churn an already-`closed` row.
    if p.status == "closed" || p.status == "archived" {
        return Ok(Some(project_id));
    }
    let has_engagement =
        onboarding_official_sets(application_store, std::slice::from_ref(&p)).await?;
    let reason = if has_engagement.contains(&p.id) {
        ClosureReason::EngagementCompleted
    } else {
        ClosureReason::PitchDeclined
    };
    transition_project_with_reason(
        application_store,
        project_id,
        Transition::Close,
        Some(reason),
        None,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(Some(project_id))
}

/// The matter lifecycle: `open` → `closed` → `archived`.
///
/// A matter's `status` is a **lifecycle** field, not a descriptive one.
/// It is coupled to `closed_at`, and the two must never disagree: open
/// matter routing keys off `status` while the ten-year retention purge
/// keys off `closed_at`, so a contradiction routes a matter as open
/// while retention treats it as closed, or the reverse.
///
/// That invariant lives in these three transitions and nowhere else.
/// [`update_project`] owns the genuinely descriptive fields and refuses
/// to touch either column — `status` and `closed_at` are rejected outright
/// by a PATCH, not silently accepted and forwarded here. Deserializes from
/// the lowercase wire form (`"close"`, `"reopen"`, `"archive"`) so
/// `POST /app/api/projects/{id}/lifecycle`, the CLI, and the
/// `close_project` MCP tool all name the same three transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    /// Close a matter directly, without the closing-letter ceremony.
    /// Stamps `closed_at` if it is not already stamped.
    Close,
    /// Reopen a closed matter. Clears `closed_at`, which restarts the
    /// retention window if it is closed again later.
    Reopen,
    /// Archive a matter. Terminal.
    Archive,
}

impl Transition {
    /// The status this transition lands the matter in.
    #[must_use]
    pub fn target_status(self) -> &'static str {
        match self {
            Transition::Close => "closed",
            Transition::Reopen => "open",
            Transition::Archive => "archived",
        }
    }
}

/// Move a matter through its lifecycle, maintaining the `status` /
/// `closed_at` invariant.
///
/// - Landing on `open` clears `closed_at`.
/// - Landing on `closed` or `archived` guarantees exactly one
///   `closed_at`. Without `effective_at`, an existing stamp is preserved;
///   a valid supplied time records when the transition actually took effect
///   and may correct an existing stamp.
///
/// **`archived` is terminal.** An archived matter refuses every
/// transition except a no-op re-archive; reopening one would resurrect a
/// matter whose retention clock is already running. Re-applying a
/// transition the matter has already made is a no-op rather than an
/// error, so a double-submitted lawyer form does not churn the row unless it
/// supplies a different valid effective time.
///
/// This is the direct lawyer path. [`close_for_notation`] is the
/// *ceremonial* path — the closing-letter workflow side effect — and routes
/// through the same lifecycle validation.
///
/// # Errors
/// [`ProjectCommandError::NotFound`] when no matter has that id, and
/// [`ProjectCommandError::Invalid`] for a transition out of `archived`, an
/// effective time on reopen, or an effective time outside the interval from
/// matter-open through now.
pub async fn transition_project(
    application_store: &ApplicationStore,
    id: Uuid,
    transition: Transition,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Project, ProjectCommandError> {
    transition_project_with_reason(application_store, id, transition, None, effective_at).await
}

/// Move a matter through its lifecycle with an optional close reason.
///
/// `reason` is required for a first close, and its allowed values are checked
/// against the matter's pre-close lifecycle class. A repeated close never
/// invents or replaces a reason already stored on the row.
pub async fn transition_project_with_reason(
    application_store: &ApplicationStore,
    id: Uuid,
    transition: Transition,
    reason: Option<ClosureReason>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Project, ProjectCommandError> {
    transition_project_attested(
        application_store,
        id,
        transition,
        reason,
        None,
        effective_at,
        None,
    )
    .await
}

/// Apply a lifecycle request with the authenticated attorney and optional reopening reason.
pub async fn transition_project_attested(
    application_store: &ApplicationStore,
    id: Uuid,
    transition: Transition,
    reason: Option<ClosureReason>,
    reopen_reason: Option<ReopenReason>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
    attesting_person_id: Option<Uuid>,
) -> Result<Project, ProjectCommandError> {
    Box::pin(transition_on_postgres(
        application_store,
        id,
        transition,
        reason,
        reopen_reason,
        effective_at,
        attesting_person_id,
    ))
    .await
}

fn lifecycle_db_error(error: impl std::fmt::Display) -> ProjectCommandError {
    ProjectCommandError::Db(error.to_string())
}

async fn lifecycle_projection(
    application_store: &ApplicationStore,
    row: &Project,
    transition: Transition,
    reason: Option<ClosureReason>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Option<String>, Option<String>), ProjectCommandError> {
    if let Some(effective_at) = effective_at {
        if effective_at > now {
            return Err(ProjectCommandError::Invalid(
                "An effective date cannot be in the future.",
            ));
        }
        let inserted_at = chrono::DateTime::parse_from_rfc3339(&row.inserted_at)
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?
            .with_timezone(&chrono::Utc);
        if effective_at < inserted_at {
            return Err(ProjectCommandError::Invalid(
                "An effective date cannot precede the matter's opened date.",
            ));
        }
    }

    // Terminal means terminal. Re-archiving is the one no-op allowed,
    // because a repeated request should not fail.
    if row.status == "archived" && transition != Transition::Archive {
        return Err(ProjectCommandError::Invalid(
            "This matter is archived; archiving is terminal.",
        ));
    }

    if transition == Transition::Close && row.status == "open" {
        let has_engagement = onboarding_official_sets(application_store, std::slice::from_ref(row))
            .await
            .map_err(ProjectCommandError::Db)?;
        let reason = reason.ok_or(ProjectCommandError::Invalid("A close reason is required."))?;
        if has_engagement.contains(&row.id) {
            if !reason.is_active() {
                return Err(ProjectCommandError::Invalid(
                    "That close reason applies only to a pitch.",
                ));
            }
        } else {
            if !reason.is_pitch() {
                return Err(ProjectCommandError::Invalid(
                    "That close reason applies only to an active matter.",
                ));
            }
            let (_, has_offboarding) =
                matter_lifecycle_sets(application_store, std::slice::from_ref(row))
                    .await
                    .map_err(ProjectCommandError::Db)?;
            if !has_offboarding.contains(&row.id) {
                return Err(ProjectCommandError::Invalid(
                    "A pitch requires an offboarding document before it can be closed.",
                ));
            }
        }
    }

    let existing_close = row.closed_at.clone();
    let closed_at = match transition {
        // Open matters carry no close date at all.
        Transition::Reopen => None,
        // An explicitly supplied date corrects the retention start even
        // when the matter already occupies the target state. Without one,
        // preserve an existing stamp; a close after reopen gets a fresh one.
        Transition::Close | Transition::Archive => Some(
            effective_at
                .map(|timestamp| timestamp.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
                .or(existing_close)
                .unwrap_or_else(|| now.to_rfc3339()),
        ),
    };
    let closure_reason = match transition {
        Transition::Reopen => None,
        Transition::Close if row.status == "open" => reason.map(|value| value.as_str().to_string()),
        Transition::Archive | Transition::Close => row.closure_reason.clone(),
    };
    Ok((closed_at, closure_reason))
}

async fn transition_on_postgres(
    application_store: &ApplicationStore,
    id: Uuid,
    transition: Transition,
    reason: Option<ClosureReason>,
    reopen_reason: Option<ReopenReason>,
    effective_at: Option<chrono::DateTime<chrono::Utc>>,
    attesting_person_id: Option<Uuid>,
) -> Result<Project, ProjectCommandError> {
    use crate::postgres::project_lifecycle as ledger;

    if transition == Transition::Reopen
        && (reopen_reason.is_none() || attesting_person_id.is_none())
    {
        return Err(ProjectCommandError::Invalid(
            "A reopen reason and an attesting Person are required.",
        ));
    }
    if transition != Transition::Reopen && reopen_reason.is_some() {
        return Err(ProjectCommandError::Invalid(
            "A reopen reason applies only to reopening a Project.",
        ));
    }
    let pg = PostgresDb::application()
        .await
        .map_err(lifecycle_db_error)?;
    find_by_id(application_store, id)
        .await
        .map_err(lifecycle_db_error)?
        .ok_or(ProjectCommandError::NotFound)?;

    let mut tx = pg.pool().begin().await.map_err(lifecycle_db_error)?;
    let row = ledger::lock(&mut tx, id)
        .await
        .map_err(lifecycle_db_error)?
        .ok_or(ProjectCommandError::NotFound)?;
    let now = chrono::Utc::now();
    let (closed_at, closure_reason) = lifecycle_projection(
        application_store,
        &row,
        transition,
        reason,
        effective_at,
        now,
    )
    .await?;
    let target = transition.target_status();
    if row.status != target || row.closed_at != closed_at || row.closure_reason != closure_reason {
        if !ledger::has_history(&mut tx, id)
            .await
            .map_err(lifecycle_db_error)?
        {
            ledger::append(
                &mut tx,
                &row,
                &lifecycle_baseline(application_store, &row).await?,
                row.closed_at.as_deref(),
                row.closure_reason.as_deref(),
            )
            .await
            .map_err(lifecycle_db_error)?;
        }
        let event = ledger::StatusTransition {
            from_status: Some(row.status.clone()),
            to_status: target.to_owned(),
            effective_at: Some(effective_at.unwrap_or(now)),
            recorded_at: Some(now),
            reason: reopen_reason
                .map(|ReopenReason::NewMatterEvent| "new_matter_event".to_owned())
                .or_else(|| {
                    closure_reason
                        .clone()
                        .filter(|_| transition == Transition::Close)
                }),
            attesting_person_id,
        };
        ledger::append(
            &mut tx,
            &row,
            &event,
            closed_at.as_deref(),
            closure_reason.as_deref(),
        )
        .await
        .map_err(lifecycle_db_error)?;
    }
    tx.commit().await.map_err(lifecycle_db_error)?;

    crate::postgres::projects::find_by_id_on(&pg, id)
        .await
        .map_err(lifecycle_db_error)?
        .ok_or(ProjectCommandError::NotFound)
}

async fn lifecycle_baseline(
    application_store: &ApplicationStore,
    project: &Project,
) -> Result<crate::postgres::project_lifecycle::StatusTransition, ProjectCommandError> {
    let mut baseline = crate::postgres::project_lifecycle::baseline(project);
    if baseline.to_status == "open" {
        let opening = crate::relationship_logs::for_project(application_store, project.id)
            .await
            .map_err(lifecycle_db_error)?
            .into_iter()
            .find(|entry| entry.action == "conflict_attestation" && entry.subject_id == project.id);
        baseline.attesting_person_id = opening.and_then(|entry| entry.actor_person_id);
    }
    Ok(baseline)
}

/// Read current lifecycle and history from the same committed PostgreSQL snapshot.
pub async fn status_with_history(
    application_store: &ApplicationStore,
    mut project: Project,
) -> Result<
    (
        Project,
        Vec<crate::postgres::project_lifecycle::StatusTransition>,
    ),
    ProjectCommandError,
> {
    use crate::postgres::project_lifecycle as ledger;
    let pg = PostgresDb::application()
        .await
        .map_err(lifecycle_db_error)?;
    let mut tx = pg.pool().begin().await.map_err(lifecycle_db_error)?;
    let current = ledger::lock(&mut tx, project.id)
        .await
        .map_err(lifecycle_db_error)?;
    let mut history = ledger::history(&mut tx, project.id)
        .await
        .map_err(lifecycle_db_error)?;
    if let Some(current) = current.filter(|_| !history.is_empty()) {
        project.status = current.status;
        project.closed_at = current.closed_at;
        project.closure_reason = current.closure_reason;
    }
    tx.commit().await.map_err(lifecycle_db_error)?;
    if history.is_empty() {
        history.push(lifecycle_baseline(application_store, &project).await?);
    }
    Ok((project, history))
}

/// Set or clear the opaque Drive folder address for a matter.
///
/// `Some` records a Google Drive resource id; `None` deliberately clears the
/// address, for example after a failed provisioning attempt is reconciled.
/// The value is not a URL, name, or sync cursor. The database's partial unique
/// index is the final guard that prevents one folder from being assigned to
/// two matters while allowing any number of rollout-state `NULL`s.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// Returns [`ProjectCommandError::Invalid`] for an empty or non-identifier
/// value, and propagates a database error (including a duplicate assignment).
pub async fn set_drive_folder_id(
    application_store: &ApplicationStore,
    project_id: Uuid,
    drive_folder_id: Option<&str>,
) -> Result<Option<Project>, ProjectCommandError> {
    let drive_folder_id = drive_folder_id
        .map(str::trim)
        .map(|id| {
            if id.is_empty()
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            {
                return Err(ProjectCommandError::Invalid(DRIVE_FOLDER_ID_INVALID));
            }
            Ok(id.to_string())
        })
        .transpose()?;

    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(
            "drive_folder_id",
            drive_folder_id.as_deref(),
        )],
    )
    .await
}

/// Record the Slack Web API channel ID provisioned for a matter's private
/// firm-side channel. This is a system-managed coordinate, not a free-form
/// resource URL, so only non-empty ASCII channel IDs are accepted here.
pub async fn set_internal_slack_channel_id(
    application_store: &ApplicationStore,
    project_id: Uuid,
    channel_id: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() || !channel_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(ProjectCommandError::Invalid(SLACK_CHANNEL_ID_INVALID));
    }
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(
            "internal_slack_channel_id",
            Some(channel_id),
        )],
    )
    .await
}

/// Record the canonical Slack URL for a matter's private firm-side channel.
/// New provider callers should use [`set_internal_slack_channel`] so the URL
/// and provider id are committed as one coordinate. This setter remains for
/// migrations and narrowly scoped callers that only repair the display URL.
///
/// # Errors
/// Returns [`ProjectCommandError::Invalid`] for an empty or malformed URL,
/// and propagates a database error.
pub async fn set_internal_slack_channel_url(
    application_store: &ApplicationStore,
    project_id: Uuid,
    channel_url: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let channel_url = channel_url.trim();
    if channel_url.is_empty() || !is_valid_resource_url(channel_url) {
        return Err(ProjectCommandError::Invalid(SLACK_CHANNEL_URL_INVALID));
    }
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(
            "internal_slack_channel_url",
            Some(channel_url),
        )],
    )
    .await
}

fn validate_internal_slack_channel<'a>(
    channel_id: &'a str,
    channel_url: &'a str,
) -> Result<(&'a str, &'a str), ProjectCommandError> {
    let channel_id = channel_id.trim();
    if channel_id.is_empty() || !channel_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(ProjectCommandError::Invalid(SLACK_CHANNEL_ID_INVALID));
    }
    let channel_url = channel_url.trim();
    if channel_url.is_empty() || !is_valid_resource_url(channel_url) {
        return Err(ProjectCommandError::Invalid(SLACK_CHANNEL_URL_INVALID));
    }
    Ok((channel_id, channel_url))
}

/// Record the same validated provider coordinate on an explicitly admitted pool.
///
/// # Errors
/// Refuses invalid coordinates, incomplete application history or storage failures.
pub async fn set_internal_slack_channel_on(
    pg: &PostgresDb,
    project_id: Uuid,
    channel_id: &str,
    channel_url: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let (channel_id, channel_url) = validate_internal_slack_channel(channel_id, channel_url)?;
    pg.require_application_history()
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    crate::postgres::projects::update_on(
        pg,
        project_id,
        &[
            crate::postgres::projects::Column::Text("internal_slack_channel_id", Some(channel_id)),
            crate::postgres::projects::Column::Text(
                "internal_slack_channel_url",
                Some(channel_url),
            ),
        ],
        None,
    )
    .await
    .map_err(|error| ProjectCommandError::Db(error.to_string()))
}
/// Record the Slack channel's id and canonical URL in one database update.
/// These two columns are one provider coordinate: exposing a row with only
/// one half would make a retry choose between posting and rendering a stale
/// link. Callers that have resolved a channel should use this atomic seam.
pub async fn set_internal_slack_channel(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    channel_id: &str,
    channel_url: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let (channel_id, channel_url) = validate_internal_slack_channel(channel_id, channel_url)?;
    let pg = authority().await?;
    set_internal_slack_channel_on(&pg, project_id, channel_id, channel_url).await
}

/// Set or clear the matter's governing jurisdiction.
///
/// A general fact about the Project — it answers whose law governs the matter
/// — read by whatever needs it, including
/// [`crate::iolta_accounts::for_project`], which derives the pooled trust
/// account from it. `None` clears it back to unsettled.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
///
/// [`ProjectCommandError::Invalid`] when `jurisdiction_id` names no
/// jurisdiction — a dangling link would make the matter's governing law
/// unreadable rather than absent — and propagates database errors.
pub async fn set_jurisdiction(
    application_store: &ApplicationStore,
    project_id: Uuid,
    jurisdiction_id: Option<Uuid>,
) -> Result<Option<Project>, ProjectCommandError> {
    if let Some(jurisdiction_id) = jurisdiction_id {
        if crate::jurisdictions::find_by_id(application_store, jurisdiction_id)
            .await
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?
            .is_none()
        {
            return Err(ProjectCommandError::Invalid(JURISDICTION_UNKNOWN));
        }
    }
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Id(
            "jurisdiction_id",
            jurisdiction_id,
        )],
    )
    .await
}

/// Refusal for a `client_dri_person_id` naming a Person who holds no
/// `is_client_dri` participation row on the matter. The column exists to pin
/// *which* client-side row a `CLIENT_DRI` special question renders (LAW-177);
/// a value pointing anywhere else would let a letter name someone the
/// accountability model never marked, so the write fails closed instead of
/// guessing.
const CLIENT_DRI_NOT_ON_PROJECT: &str = "client_dri_person_id names a Person with no client-DRI \
 participation row on this matter — designate one first";

/// Set or clear the Project's client DRI Person (LAW-177).
///
/// `Some(person_id)` must name a Person who exists and holds an
/// `is_client_dri` participation row on *this* matter — the column only pins
/// an accountability the participation model already marked, never invents
/// one. `None` clears the column. The PostgreSQL foreign key is the backstop
/// for a writer that skips this accessor.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// [`ProjectCommandError::Invalid`] when the Person is unknown or holds no
/// client-DRI row on the matter, and propagates a database error.
pub async fn set_client_dri_person_id(
    application_store: &ApplicationStore,
    project_id: Uuid,
    person_id: Option<Uuid>,
) -> Result<Option<Project>, ProjectCommandError> {
    if let Some(person_id) = person_id {
        if crate::persons::find_by_id(application_store, person_id)
            .await
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?
            .is_none()
        {
            return Err(ProjectCommandError::Invalid("no such person"));
        }
        let on_this_matter = participation_for_person(application_store, person_id, project_id)
            .await
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?
            .is_some_and(|row| row.is_client_dri);
        if !on_this_matter {
            return Err(ProjectCommandError::Invalid(CLIENT_DRI_NOT_ON_PROJECT));
        }
    }
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Id(
            "client_dri_person_id",
            person_id,
        )],
    )
    .await
}

/// The legal-plan panel columns staged onto a plan matter (ENG-1269). The
/// intake path (ENG-1270) sets them when the attorney accepts a staged
/// matter; every field is optional and a `None` clears the column. The ten
/// land in one patch so a staged matter never reads with half its plan
/// identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanColumns {
    /// The Firm's `legal_plan` row this matter works under. A bare UUID:
    /// the table is authored in PostgreSQL, and the SurrealDB source keeps
    /// the value plain.
    pub legal_plan_id: Option<Uuid>,
    pub plan_member_id: Option<String>,
    /// The plan's own number for this referral, verbatim — what the plan's
    /// emails and claims cite.
    pub plan_case_number: Option<String>,
    pub benefit_code: Option<String>,
    /// A word from the closed [`crate::legal_plans::PlanMatterType`]
    /// vocabulary — `as_str()`, so `plan_fee_schedule` and the assumption
    /// keys match on it.
    pub plan_matter_type: Option<String>,
    /// A lowercase jurisdiction code, as [`plan_fee_schedule.state`] holds.
    ///
    /// [`plan_fee_schedule.state`]: crate::legal_plans::PlanFeeSchedule
    pub plan_state: Option<String>,
    /// ISO `YYYY-MM-DD` (the mirror's DATE parse rule).
    pub plan_assigned_on: Option<String>,
    pub plan_accepted_on: Option<String>,
    pub plan_closed_on: Option<String>,
    /// Which kit version this matter applied (ENG-1271).
    pub kit_version: Option<String>,
}

impl PlanColumns {
    /// The plan columns one Project carries now — the base a partial
    /// update overlays, so a caller changing one field keeps the rest.
    #[must_use]
    pub fn of(project: &Project) -> Self {
        Self {
            legal_plan_id: project.legal_plan_id,
            plan_member_id: project.plan_member_id.clone(),
            plan_case_number: project.plan_case_number.clone(),
            benefit_code: project.benefit_code.clone(),
            plan_matter_type: project.plan_matter_type.clone(),
            plan_state: project.plan_state.clone(),
            plan_assigned_on: project.plan_assigned_on.clone(),
            plan_accepted_on: project.plan_accepted_on.clone(),
            plan_closed_on: project.plan_closed_on.clone(),
            kit_version: project.kit_version.clone(),
        }
    }
}

const PLAN_MATTER_TYPE_INVALID: &str =
    "plan_matter_type is not one of document_review, letter, uncontested_divorce, court_appearance, contested_divorce, custody, probate";
const PLAN_STATE_INVALID: &str = "plan_state is not a lowercase jurisdiction code";
const PLAN_DATE_INVALID: &str = "plan date is not an ISO YYYY-MM-DD date";
const PLAN_CASE_NUMBER_INVALID: &str =
    "plan_case_number must be 1 to 64 visible characters with no spaces";
const PLAN_UNKNOWN: &str = "legal_plan_id does not name a legal plan";
const PLAN_FIRM_MISMATCH: &str = "legal_plan_id names a plan of another firm";

/// Validates one plan date. ISO `YYYY-MM-DD` only, the shape a PostgreSQL
/// `DATE` column stores.
fn valid_plan_date(text: &str) -> bool {
    chrono::NaiveDate::parse_from_str(text, "%F")
        .is_ok_and(|date| date.format("%F").to_string() == text)
}

/// A Plan Case Number as the plan prints it (`4963680-001`, `412590`): one
/// token, so an inbox rule can match it as a whole word in a subject line.
pub(crate) fn valid_plan_case_number(text: &str) -> bool {
    (1..=64).contains(&text.chars().count()) && text.chars().all(|c| c.is_ascii_graphic())
}

fn check_plan_columns(columns: &PlanColumns) -> Result<(), ProjectCommandError> {
    if let Some(matter_type) = &columns.plan_matter_type {
        if crate::legal_plans::PlanMatterType::parse(matter_type).is_err() {
            return Err(ProjectCommandError::Invalid(PLAN_MATTER_TYPE_INVALID));
        }
    }
    if let Some(state) = &columns.plan_state {
        if !crate::legal_plans::valid_state_code(state) {
            return Err(ProjectCommandError::Invalid(PLAN_STATE_INVALID));
        }
    }
    if let Some(case_number) = &columns.plan_case_number {
        if !valid_plan_case_number(case_number) {
            return Err(ProjectCommandError::Invalid(PLAN_CASE_NUMBER_INVALID));
        }
    }
    for text in [
        &columns.plan_assigned_on,
        &columns.plan_accepted_on,
        &columns.plan_closed_on,
    ]
    .into_iter()
    .flatten()
    {
        if !valid_plan_date(text) {
            return Err(ProjectCommandError::Invalid(PLAN_DATE_INVALID));
        }
    }
    Ok(())
}

/// Writes the legal-plan panel columns onto one Project in one patch
/// (ENG-1269). Passing `None` for a field clears it, so the same setter
/// stages an assignment, records acceptance, and records the applied kit.
/// A plan id must name one of the Firm's own `legal_plan` rows; the matter
/// type and state follow the same closed rules `plan_fee_schedule` enforces.
///
/// # Errors
/// [`ProjectCommandError::Invalid`] for a matter type outside the closed
/// vocabulary, a malformed state or date, or a plan of another Firm, and
/// propagates a database error.
///
/// [`ProjectCommandError::Invalid`] names the field; the PostgreSQL foreign
/// key is the backstop for a writer that skips this accessor.
pub async fn set_plan_columns(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    project_id: Uuid,
    columns: &PlanColumns,
) -> Result<Option<Project>, ProjectCommandError> {
    set_plan_columns_with_benefit(application_store, pg, project_id, columns, None).await
}

/// Update the plan and its verified benefit in the same Project write.
///
/// # Errors
/// Refuses inconsistent benefit terms, a plan outside the Firm, and database failures.
pub async fn set_plan_columns_with_benefit(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    project_id: Uuid,
    columns: &PlanColumns,
    terms: Option<&model::plan_benefit::PlanBenefitTerms>,
) -> Result<Option<Project>, ProjectCommandError> {
    use crate::postgres::projects::Column::{Date, Id, Text};

    if let Some(terms) = terms {
        terms.validate().map_err(ProjectCommandError::Invalid)?;
        if columns.legal_plan_id.is_none() || columns.benefit_code.is_none() {
            return Err(ProjectCommandError::Invalid(
                "benefit terms require a Legal Plan and benefit code",
            ));
        }
    }
    check_plan_columns(columns)?;
    let Some(project) = find_by_id(application_store, project_id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
    else {
        return Ok(None);
    };
    if let Some(plan_id) = columns.legal_plan_id {
        let plan_firm = crate::legal_plans::firm_of(pg, plan_id)
            .await
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
        match plan_firm {
            Some(firm_id) if project.firm_id == Some(firm_id) => {}
            Some(_) => return Err(ProjectCommandError::Invalid(PLAN_FIRM_MISMATCH)),
            None => return Err(ProjectCommandError::Invalid(PLAN_UNKNOWN)),
        }
    }
    let authority = authority().await?;
    let date = |text: &Option<String>| text.as_deref().and_then(|text| text.parse().ok());
    let mut updates = vec![
        Id("legal_plan_id", columns.legal_plan_id),
        Text("plan_member_id", columns.plan_member_id.as_deref()),
        Text("plan_case_number", columns.plan_case_number.as_deref()),
        Text("benefit_code", columns.benefit_code.as_deref()),
        Text("plan_matter_type", columns.plan_matter_type.as_deref()),
        Text("plan_state", columns.plan_state.as_deref()),
        Date("plan_assigned_on", date(&columns.plan_assigned_on)),
        Date("plan_accepted_on", date(&columns.plan_accepted_on)),
        Date("plan_closed_on", date(&columns.plan_closed_on)),
        Text("kit_version", columns.kit_version.as_deref()),
    ];
    updates.extend(plan_benefit_columns(columns, terms));
    let within_firm = project.firm_id.map(|firm| (firm, None));
    Ok(patch_in_postgres(
        application_store,
        &authority,
        project_id,
        &updates,
        within_firm,
    )
    .await?)
}

fn plan_benefit_columns<'a>(
    columns: &'a PlanColumns,
    terms: Option<&model::plan_benefit::PlanBenefitTerms>,
) -> [crate::postgres::projects::Column<'a>; 6] {
    use crate::postgres::projects::Column::{Integer, KeepPlanBenefit, Text};

    if let Some(terms) = terms {
        [
            Text("plan_cap_shape", Some(terms.cap_shape.as_str())),
            Integer("plan_cap_cents", terms.cap_cents),
            Integer("plan_cap_minutes", terms.cap_minutes),
            Integer("plan_contested_cap_cents", terms.contested_cap_cents),
            Integer("plan_rate_cents_per_hour", terms.plan_rate_cents_per_hour),
            Integer(
                "plan_paralegal_rate_cents_per_hour",
                terms.paralegal_rate_cents_per_hour,
            ),
        ]
    } else {
        [
            "plan_cap_shape",
            "plan_cap_cents",
            "plan_cap_minutes",
            "plan_contested_cap_cents",
            "plan_rate_cents_per_hour",
            "plan_paralegal_rate_cents_per_hour",
        ]
        .map(|name| KeepPlanBenefit(name, columns.legal_plan_id, columns.benefit_code.as_deref()))
    }
}

/// Link a manifest's Firm-owned Legal Plan, leaving existing plan details intact.
pub async fn link_manifest_legal_plan(
    application_store: &ApplicationStore,
    project: &Project,
    firm: Uuid,
    brand: &str,
    plan: Uuid,
    dry_run: bool,
) -> Result<Option<Project>, ProjectCommandError> {
    const CONFLICT: &str = "the Project already links a different Legal Plan";
    if project.firm_id != Some(firm) || project.brand != brand {
        return Err(ProjectCommandError::NotFound);
    }
    match project.legal_plan_id {
        Some(id) if id == plan => return Ok(Some(project.clone())),
        Some(_) => return Err(ProjectCommandError::Invalid(CONFLICT)),
        None if dry_run => return Ok(Some(project.clone())),
        None => {}
    }
    let updated = {
        let pg = authority().await?;
        crate::postgres::projects::link_legal_plan_on(&pg, project.id, firm, brand, plan)
            .await
            .map_err(ProjectStoreError::from)?
    };
    if updated.is_some() {
        return Ok(updated);
    }
    let current = find_by_id(application_store, project.id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    match current {
        Some(row) if row.firm_id == Some(firm) && row.brand == brand => {
            if row.legal_plan_id == Some(plan) {
                Ok(Some(row))
            } else {
                Err(ProjectCommandError::Invalid(CONFLICT))
            }
        }
        _ => Ok(None),
    }
}

/// Record the firm-private Notion page provisioned for a Project. This is a
/// system-managed coordinate; client-facing project payloads must redact it.
pub async fn set_private_notion_page_url(
    application_store: &ApplicationStore,
    project_id: Uuid,
    page_url: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let page_url = page_url.trim();
    if page_url.is_empty() || !is_valid_resource_url(page_url) {
        return Err(ProjectCommandError::Invalid(NOTION_PAGE_URL_INVALID));
    }
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(
            "private_notion_page_url",
            Some(page_url),
        )],
    )
    .await
}

/// Record the shared Google Calendar provisioned for a Project. Like the
/// firm-private Notion page, it is a system-managed coordinate written by
/// `navigator project setup` before the setup reports success.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// [`ProjectCommandError::Invalid`] for a value [`is_valid_calendar_id`]
/// refuses, or a database error.
pub async fn set_calendar_id(
    application_store: &ApplicationStore,
    project_id: Uuid,
    calendar_id: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let calendar_id = calendar_id.trim();
    if !is_valid_calendar_id(calendar_id) {
        return Err(ProjectCommandError::Invalid(CALENDAR_ID_INVALID));
    }
    set_optional_column(
        application_store,
        project_id,
        "calendar_id",
        Some(calendar_id),
    )
    .await
}

/// Clear the client's booking link. Closing a matter calls this: a closed
/// matter takes no new bookings.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// Propagates a database error.
pub async fn clear_booking_url(
    application_store: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<Project>, ProjectCommandError> {
    set_optional_column(application_store, project_id, "booking_url", None).await
}

async fn set_optional_column(
    application_store: &ApplicationStore,
    project_id: Uuid,
    column: &'static str,
    value: Option<&str>,
) -> Result<Option<Project>, ProjectCommandError> {
    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(column, value)],
    )
    .await
}

/// Request body for updating a matter's **descriptive** fields through the
/// command boundary — its name, entity, and scope narrative. Deliberately
/// narrow: it is neither the matter-open path (no conflict check, no repo
/// provisioning) nor a lifecycle transition. `status` and its coupled
/// `closed_at` are intentionally absent — moving a matter through
/// open/closed/archived is a lifecycle change whose retention semantics are a
/// firm-policy determination, so it belongs to [`transition_project`] through
/// `POST /app/api/projects/{id}/lifecycle`, not this general edit.
///
/// `#[serde(deny_unknown_fields)]` is load-bearing here, not decorative: a
/// caller posting `status` or `closed_at` (or any other field this struct
/// does not name) must be told so with a `400`, rather than have the field
/// silently dropped while a `200` implies it was honored.
///
/// # This is always a patch
///
/// **Every field is optional, and an absent field is never written.** A caller
/// sends the fields it wants to change and nothing else; the columns it omits
/// keep the values they had. There is no field whose absence means "blank this
/// out", because that is how a partial update silently destroys data a caller
/// never mentioned — a client that reads a matter, edits one field, and sends
/// its own narrow body should not be able to erase the four collaboration
/// links by not knowing about them.
///
/// So the three states a column can be in are:
///
/// | The body | Effect |
/// | --- | --- |
/// | the field is absent (or `null`) | the column is left exactly as it was |
/// | the field is `""` | the column is cleared |
/// | the field has a value | the column is set to it |
///
/// `null` and absent are deliberately the same. `serde` cannot distinguish them
/// on an `Option` without a nested `Option`, and collapsing them toward *leave
/// alone* is the safe direction: the failure mode of the other choice is a
/// caller wiping a column it did not intend to touch. Clearing is therefore
/// always explicit, and always the empty string — the same value an HTML form
/// posts for a text input a person emptied, so the form and the JSON caller
/// converge on one rule rather than each getting its own.
///
/// `name` is the one field that refuses `""`, because a matter with no name is
/// not a state a patch may produce. Omitting it leaves the name alone.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectCommand {
    /// The matter's name. Absent leaves it alone, like every other field here;
    /// present-but-blank is refused, because a matter with no name is not a
    /// state a patch may produce.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub entity_id: Option<Uuid>,
    /// The Firm and brand that own this Project, updated together. Either
    /// both are absent or both name a pair in `firm_brand`.
    #[serde(default)]
    pub firm_id: Option<Uuid>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub internal_slack_channel_url: Option<String>,
    #[serde(default)]
    pub external_slack_channel_url: Option<String>,
    /// The Project's source repository as a whole URL, on any forge. A blank
    /// submission clears it; an omitted one leaves it untouched.
    #[serde(default)]
    pub repository_url: Option<String>,
    /// The firm-only Notion page. A blank submission clears it; an omitted one
    /// leaves it untouched.
    #[serde(default)]
    pub private_notion_page_url: Option<String>,
    /// The client-shared Notion page, on the same blank-clears terms.
    #[serde(default)]
    pub shared_notion_page_url: Option<String>,
    /// The Project's shared Google Calendar id, on the same blank-clears
    /// terms. Checked by [`is_valid_calendar_id`].
    #[serde(default)]
    pub calendar_id: Option<String>,
    /// Explicit IANA time zone for this Project. A blank update restores the derived default.
    #[serde(default)]
    pub calendar_timezone: Option<String>,
    /// The Project's Drive ingest folder id, on the same blank-clears terms.
    /// Checked by [`is_valid_drive_folder_id`] for shape and by
    /// [`drive_folder_recorded_elsewhere`] for ownership — a folder another
    /// matter already imports is refused, never silently shared.
    #[serde(default)]
    pub drive_folder_id: Option<String>,
    /// The client's booking link, on the same blank-clears terms. Rendered as
    /// an `href` to the client, so it passes [`is_valid_resource_url`].
    #[serde(default)]
    pub booking_url: Option<String>,
    /// The onboarding jurisdiction a Clause input may bind directly (the
    /// LAW-228 replacement for the questionnaire-state binding hack). Set by
    /// `navigator project set --jurisdiction`. A blank submission clears it;
    /// an omitted one leaves it untouched.
    #[serde(default)]
    pub onboarding_jurisdiction: Option<String>,
    /// The Kit version this matter used, `{jurisdiction}/{practice_area}/
    /// {version}` — written through by `navigator project skill use` when it
    /// pins the same pair into the Project's `navigator.yaml`, so every
    /// matter reports which Kit produced its work. A blank submission clears
    /// it; an omitted one leaves it untouched. Checked by
    /// [`is_valid_kit_version`].
    #[serde(default)]
    pub kit_version: Option<String>,
}

/// Set or clear the Project's source repository URL.
///
/// The provisioning-side counterpart to the `repository_url` field on
/// [`UpdateProjectCommand`], for a caller that holds only the matter id — the
/// dev seed, or a reconciler recording where a Project's source landed.
/// `None` clears the column.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// Returns [`ProjectCommandError::Invalid`] when the URL is not one
/// [`is_valid_repository_url`] accepts, and propagates a database error.
pub async fn set_repository_url(
    application_store: &ApplicationStore,
    project_id: Uuid,
    repository_url: Option<&str>,
) -> Result<Option<Project>, ProjectCommandError> {
    let repository_url = repository_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| {
            if is_valid_repository_url(url) {
                Ok(url.to_string())
            } else {
                Err(ProjectCommandError::Invalid(REPOSITORY_URL_INVALID))
            }
        })
        .transpose()?;

    patch_under_authority(
        application_store,
        project_id,
        &[crate::postgres::projects::Column::Text(
            "repository_url",
            repository_url.as_deref(),
        )],
    )
    .await
}

/// Record a provisioned repository's URL and when it was provisioned in one
/// statement, only on a row that records no repository yet. A crash leaves
/// neither or both, and a concurrent writer's URL is never replaced: the
/// returned row carries whichever URL won.
///
/// Returns `Ok(None)` when the matter no longer exists.
///
/// # Errors
/// [`ProjectCommandError::Invalid`] for a URL [`is_valid_repository_url`]
/// refuses, or a database error.
pub async fn record_provisioned_repository(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    repository_url: &str,
    provisioned_at: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    if !is_valid_repository_url(repository_url.trim()) {
        return Err(ProjectCommandError::Invalid(REPOSITORY_URL_INVALID));
    }
    let pg = authority().await?;
    let project = Box::pin(crate::postgres::projects::record_provisioned_repository_on(
        &pg,
        project_id,
        repository_url,
        provisioned_at,
    ))
    .await
    .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    Ok(project)
}

/// Record that a verified first commit initialized the Project's source at
/// `repository_url`, in one statement: the URL (only where none or the same
/// one is recorded), `forge_provisioned_at` (only where unset), and
/// `git_initialized_at`. Returns the row as stored; a caller compares its URL
/// to detect a pointer another writer recorded.
///
/// # Errors
/// [`ProjectCommandError::Invalid`] for a refused URL, or a database error.
pub async fn record_source_initialized(
    _application_store: &ApplicationStore,
    project_id: Uuid,
    repository_url: &str,
    initialized_at: &str,
) -> Result<Option<Project>, ProjectCommandError> {
    let url = repository_url.trim();
    if !is_valid_repository_url(url) {
        return Err(ProjectCommandError::Invalid(REPOSITORY_URL_INVALID));
    }
    let pg = authority().await?;
    let project = Box::pin(crate::postgres::projects::record_source_initialized_on(
        &pg,
        project_id,
        repository_url,
        initialized_at,
    ))
    .await
    .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    Ok(project)
}

/// A matter's descriptive update could not be applied.
#[derive(Debug, thiserror::Error)]
pub enum ProjectCommandError {
    /// The request is malformed in a way the caller can correct.
    #[error("{0}")]
    Invalid(&'static str),
    /// No matter with the requested id.
    #[error("no matter with that id")]
    NotFound,
    /// The database refused a delete because other rows still reference this
    /// matter (participations, notations, price events, …). Carries the
    /// database's own detail, which names the referencing table so an
    /// operator sees *why*. Caller-correctable (detach/close those first), so
    /// an adapter renders it as a conflict, not a server fault.
    #[error("this matter is still referenced by other records ({0})")]
    Referenced(String),
    #[error("database: {0}")]
    Db(String),
}

/// Refuse any collaboration resource link that would not navigate.
///
/// Split out of [`update_project`] so the command body stays about *what* it
/// writes. Each of the four is rendered as an `href` on the matter page, to
/// lawyer and client alike, so the gate is the same for all of them — a panel
/// that trusted one column more than another would be a gap waiting for the
/// next field to be added to the wrong half. A blank value is the documented
/// way to clear a column, so it passes.
/// The optional text columns [`update_project`] may set, each paired with the
/// command field it reads.
///
/// One table drives both halves of the sparse update — which assignments the
/// `SET` clause carries and which parameters are bound — so a new optional
/// column is one row here instead of two `if` blocks that must agree. `None`
/// means "leave this column alone"; `Some("")` means "clear it".
fn optional_text_columns(input: &UpdateProjectCommand) -> [(&'static str, Option<&String>); 12] {
    [
        ("description", input.description.as_ref()),
        (
            "internal_slack_channel_url",
            input.internal_slack_channel_url.as_ref(),
        ),
        (
            "external_slack_channel_url",
            input.external_slack_channel_url.as_ref(),
        ),
        ("repository_url", input.repository_url.as_ref()),
        (
            "private_notion_page_url",
            input.private_notion_page_url.as_ref(),
        ),
        (
            "shared_notion_page_url",
            input.shared_notion_page_url.as_ref(),
        ),
        ("calendar_id", input.calendar_id.as_ref()),
        ("calendar_timezone", input.calendar_timezone.as_ref()),
        ("drive_folder_id", input.drive_folder_id.as_ref()),
        ("booking_url", input.booking_url.as_ref()),
        (
            "onboarding_jurisdiction",
            input.onboarding_jurisdiction.as_ref(),
        ),
        ("kit_version", input.kit_version.as_ref()),
    ]
}

fn validate_resource_links(input: &UpdateProjectCommand) -> Result<(), ProjectCommandError> {
    let links = [
        &input.internal_slack_channel_url,
        &input.external_slack_channel_url,
        &input.private_notion_page_url,
        &input.shared_notion_page_url,
        &input.booking_url,
    ];
    for url in links.into_iter().flatten() {
        if !url.trim().is_empty() && !is_valid_resource_url(url) {
            return Err(ProjectCommandError::Invalid(RESOURCE_URL_INVALID));
        }
    }
    if let Some(zone) = input.calendar_timezone.as_deref().map(str::trim) {
        if !zone.is_empty() && zone.parse::<chrono_tz::Tz>().is_err() {
            return Err(ProjectCommandError::Invalid(
                "calendar_timezone must be an IANA time zone",
            ));
        }
    }
    if let Some(calendar_id) = &input.calendar_id {
        if !calendar_id.trim().is_empty() && !is_valid_calendar_id(calendar_id) {
            return Err(ProjectCommandError::Invalid(CALENDAR_ID_INVALID));
        }
    }
    if let Some(drive_folder_id) = &input.drive_folder_id {
        if !drive_folder_id.trim().is_empty() && !is_valid_drive_folder_id(drive_folder_id) {
            return Err(ProjectCommandError::Invalid(DRIVE_FOLDER_ID_INVALID));
        }
    }
    Ok(())
}

/// Refusal for a Firm/brand pair whose key no `brand` row carries.
const UNKNOWN_BRAND_KEY: &str = "No Brand has that key.";
/// Refusal for a brand key that exists but no `firm_brand` row wears.
const BRAND_WORN_BY_NO_FIRM: &str = "That brand is not worn by a Firm.";
/// Refusal for a brand key a different Firm wears. A key belongs to at most
/// one Firm (`firm_brand.brand_key` is unique), so the key implies its Firm.
const BRAND_WORN_BY_ANOTHER_FIRM: &str = "That brand is worn by a different Firm.";

async fn validate_firm_brand_update(
    application_store: &ApplicationStore,
    input: &UpdateProjectCommand,
) -> Result<Option<(Uuid, String)>, ProjectCommandError> {
    match (input.firm_id, input.brand.as_deref()) {
        (None, None) => Ok(None),
        (Some(firm_id), Some(brand)) => {
            let brand = brand.trim();
            if brand.is_empty() {
                return Err(ProjectCommandError::Invalid("Brand is required."));
            }
            // Name which case refused the pair (LAW-255): a key no `brand`
            // row carries, a key no Firm wears, or a key another Firm wears.
            // `find_by_key` reads the same store `firm_id_for_brand_key`
            // does, so the two checks agree under PostgreSQL tenancy too.
            if crate::brands::find_by_key(application_store, brand)
                .await
                .map_err(|error| ProjectCommandError::Db(error.to_string()))?
                .is_none()
            {
                return Err(ProjectCommandError::Invalid(UNKNOWN_BRAND_KEY));
            }
            let wearing_firm = crate::firms::firm_id_for_brand_key(application_store, brand)
                .await
                .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
            match wearing_firm {
                Some(wearer) if wearer == firm_id => Ok(Some((firm_id, brand.to_string()))),
                Some(_) => Err(ProjectCommandError::Invalid(BRAND_WORN_BY_ANOTHER_FIRM)),
                None => Err(ProjectCommandError::Invalid(BRAND_WORN_BY_NO_FIRM)),
            }
        }
        _ => Err(ProjectCommandError::Invalid(
            "Firm and brand must be set together.",
        )),
    }
}

/// Validate the request-only update invariants: required name, the
/// repository URL's clone-and-render safety, resource links, and the Kit
/// version's three-segment shape. Every check is a pure function of
/// `input`, so the command can fail before any read or write.
fn validate_update_command(input: &UpdateProjectCommand) -> Result<(), ProjectCommandError> {
    if let Some(name) = &input.name {
        if name.trim().is_empty() {
            return Err(ProjectCommandError::Invalid("Name is required."));
        }
    }
    // A blank submission clears the column; anything else must be a URL that
    // could actually be cloned and safely rendered as a link.
    if let Some(url) = &input.repository_url {
        if !url.trim().is_empty() && !is_valid_repository_url(url) {
            return Err(ProjectCommandError::Invalid(REPOSITORY_URL_INVALID));
        }
    }
    validate_resource_links(input)?;
    // A Kit version is displayed as recorded and reported per matter, so a
    // value that is not three path segments is refused rather than normalized.
    if let Some(kit_version) = &input.kit_version {
        if !kit_version.trim().is_empty() && !is_valid_kit_version(kit_version.trim()) {
            return Err(ProjectCommandError::Invalid(
                "kit_version must be {jurisdiction}/{practice_area}/{version}, three non-empty \
                 segments with no whitespace.",
            ));
        }
    }
    Ok(())
}

/// Validate the row-dependent update references: the matter exists, a
/// repointed entity exists, and a named Drive ingest folder is not recorded
/// on another matter.
async fn validate_update_references(
    application_store: &ApplicationStore,
    id: Uuid,
    input: &UpdateProjectCommand,
) -> Result<(), ProjectCommandError> {
    if find_by_id(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
        .is_none()
    {
        return Err(ProjectCommandError::NotFound);
    }
    if let Some(entity_id) = input.entity_id {
        // A `record<entity>` link is not validated by the engine, so this
        // read-back is what keeps a matter from being repointed at an
        // entity that does not exist.
        if crate::entities::find_by_id(application_store, entity_id)
            .await
            .map_err(|error| ProjectCommandError::Db(error.to_string()))?
            .is_none()
        {
            return Err(ProjectCommandError::Invalid("That entity does not exist."));
        }
    }
    // A Drive ingest folder scopes its import to exactly one matter, so a
    // patch naming a folder another matter already records is refused at the
    // command seam rather than discovered as a misdirected import later.
    if let Some(drive_folder_id) = input.drive_folder_id.as_deref() {
        if !drive_folder_id.trim().is_empty()
            && drive_folder_recorded_elsewhere(application_store, id, drive_folder_id.trim())
                .await
                .map_err(probe_failed)?
        {
            return Err(ProjectCommandError::Invalid(
                DRIVE_FOLDER_RECORDED_ELSEWHERE,
            ));
        }
    }
    Ok(())
}

/// Update a matter's descriptive fields — name, entity, scope narrative, its
/// Slack channels, and its source repository URL. Behind both the JSON
/// `PATCH /app/api/projects/{id}` command and the `/app/projects/{id}` edit
/// form, so neither door re-implements the write. Name is required; a submitted
/// `entity_id` or `description` is applied and an omitted one is left
/// untouched, with a blank description clearing the column.
///
/// `repository_url` is the one Project field that is validated rather than
/// merely trimmed ([`is_valid_repository_url`]): it is handed to `git clone`
/// and rendered as a link, so a bad scheme or an embedded credential is
/// refused here rather than stored.
///
/// Scope is deliberately narrow. It is not the matter-open path (no conflict
/// check), it does not move the agreed price (the append-only price-events
/// command), and it does not change `status`/`closed_at` — a lifecycle
/// transition whose retention semantics are a firm-policy determination owned
/// by [`transition_project`].
///
/// Because every written value comes wholly from the request (never from a
/// read of the row), there is no read-modify-write to serialize: the sparse
/// `Unchanged(id)` update leaves `status`, `closed_at`, and every other
/// column a concurrent lifecycle write may be changing entirely untouched.
pub async fn update_project(
    application_store: &ApplicationStore,
    id: Uuid,
    input: &UpdateProjectCommand,
) -> Result<Project, ProjectCommandError> {
    update_project_inner(application_store, id, input, false).await
}

/// Change the Brand within the Project's current Firm, even if another
/// administrator moves the Project between the caller's read and this write.
pub async fn change_brand(
    application_store: &ApplicationStore,
    id: Uuid,
    firm_id: Uuid,
    brand: String,
) -> Result<Project, ProjectCommandError> {
    update_project_inner(
        application_store,
        id,
        &UpdateProjectCommand {
            firm_id: Some(firm_id),
            brand: Some(brand),
            ..Default::default()
        },
        true,
    )
    .await
}

/// [`update_project_inner`]'s write, PostgreSQL first.
async fn update_project_in_postgres(
    application_store: &ApplicationStore,
    pg: &PostgresDb,
    id: Uuid,
    input: &UpdateProjectCommand,
    firm_brand: Option<&(Uuid, String)>,
    keep_firm: bool,
) -> Result<Project, ProjectCommandError> {
    use crate::postgres::projects::Column::{Id, Text};
    // A blank submission clears the column rather than storing "".
    let texts: Vec<(&'static str, Option<String>)> = optional_text_columns(input)
        .into_iter()
        .filter_map(|(column, value)| {
            value.map(|value| (column, crate::people_commands::none_if_blank(Some(value))))
        })
        .collect();
    let mut columns = Vec::new();
    if let Some(name) = input.name.as_deref() {
        columns.push(Text("name", Some(name.trim())));
    }
    if let Some(entity_id) = input.entity_id {
        columns.push(Id("entity_id", Some(entity_id)));
    }
    if let Some((firm_id, brand)) = firm_brand {
        if !keep_firm {
            columns.push(Id("firm_id", Some(*firm_id)));
        }
        columns.push(Text("brand", Some(brand)));
    }
    columns.extend(
        texts
            .iter()
            .map(|(column, value)| Text(column, value.as_deref())),
    );
    let within_firm = firm_brand
        .filter(|_| keep_firm)
        .map(|(firm_id, brand)| (*firm_id, Some(brand.as_str())));
    patch_in_postgres(application_store, pg, id, &columns, within_firm)
        .await?
        .ok_or(ProjectCommandError::NotFound)
}

async fn update_project_inner(
    application_store: &ApplicationStore,
    id: Uuid,
    input: &UpdateProjectCommand,
    keep_firm: bool,
) -> Result<Project, ProjectCommandError> {
    validate_update_command(input)?;
    let firm_brand = validate_firm_brand_update(application_store, input).await?;
    validate_update_references(application_store, id, input).await?;
    // The optional text columns are handled from one table, so a column is set
    // and bound from the same row rather than from two `if` blocks a hundred
    // lines apart that have to be kept in step.
    let pg = authority().await?;
    update_project_in_postgres(
        application_store,
        &pg,
        id,
        input,
        firm_brand.as_ref(),
        keep_firm,
    )
    .await
}

/// A reference probe could not be answered. The matter is left alone: a
/// guard that cannot read is not a guard that passed.
fn probe_failed(error: impl std::fmt::Display) -> ProjectCommandError {
    ProjectCommandError::Db(error.to_string())
}

/// Delete a matter after validating the records in the projects cluster.
/// Nothing in the engine refuses a delete that strands a reference, so the
/// check belongs at this command seam.
pub async fn delete_project(
    application_store: &ApplicationStore,
    id: Uuid,
) -> Result<Project, ProjectCommandError> {
    let project = find_by_id(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
        .ok_or(ProjectCommandError::NotFound)?;
    if !participations_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
        .is_empty()
    {
        return Err(ProjectCommandError::Referenced(
            "person_project_roles".into(),
        ));
    }
    // `notations` is Surreal-resident, so its reference check runs against
    // that engine.
    if crate::notations::exists_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
    {
        return Err(ProjectCommandError::Referenced("notations".into()));
    }
    if !crate::xero_invoices::for_projects(application_store, &[id])
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?
        .is_empty()
    {
        return Err(ProjectCommandError::Referenced("xero_invoice".into()));
    }
    // The remaining five reference checks, each asked of the module that
    // owns the table. All five moved to SurrealDB — `templates` and
    // `assets` with ENG-121, `communications`, `expunge_requests`, and
    // `expunge_records` with ENG-160 — so each answer comes from the engine
    // that actually holds the rows.
    if crate::templates::exists_for_project(application_store, id)
        .await
        .map_err(probe_failed)?
    {
        return Err(ProjectCommandError::Referenced("templates".into()));
    }
    if crate::expunge_requests::exists_for_project(application_store, id)
        .await
        .map_err(probe_failed)?
    {
        return Err(ProjectCommandError::Referenced("expunge_requests".into()));
    }
    if crate::communications::exists_for_project(application_store, id)
        .await
        .map_err(probe_failed)?
    {
        return Err(ProjectCommandError::Referenced("communications".into()));
    }
    if crate::expunge_records::exists_for_project(application_store, id)
        .await
        .map_err(probe_failed)?
    {
        return Err(ProjectCommandError::Referenced("expunge_records".into()));
    }
    if crate::assets::exists_for_project(application_store, id)
        .await
        .map_err(probe_failed)?
    {
        return Err(ProjectCommandError::Referenced("assets".into()));
    }
    // Surreal cascades nothing, so the
    // `authority_uses`/`citations`/`verifications` chain is walked
    // explicitly before the project row itself is removed below.
    crate::authorities::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    // `cases` and its discovery chain moved to SurrealDB with wave six
    // (ENG-160); the cascade walks that chain explicitly, the same way the
    // `authorities` sweep above does.
    crate::cases::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    // The synced Matter Pages and Matter Caption are a copy of the
    // repository's manifest, so they go with the row rather than blocking
    // the delete.
    crate::matter_pages::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    crate::matter_captions::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    // Fiduciary roles, the estate asset schedule, and cap table holdings are
    // imported from the repository's seeds, so they go with the row too.
    crate::fiduciary_roles::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    crate::estate_assets::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    crate::cap_table_holdings::delete_for_project(application_store, id)
        .await
        .map_err(|error| ProjectCommandError::Db(error.to_string()))?;
    let pg = authority().await?;
    let pg = &pg;
    Box::pin(crate::postgres::projects::delete_on(pg, id))
        .await
        .map_err(ProjectStoreError::from)?;
    Ok(project)
}

/// Everything an adapter has resolved from its caller's session and form to
/// open a matter. The session-dependent parts — which client, which entity,
/// and *who is attesting* — are resolved by the adapter (web
/// form, CLI, or `POST /app/api/projects`) and handed here as ids, so the command
/// itself stays free of session and HTTP types.
#[derive(Debug)]
pub struct OpenMatterCommand {
    /// The authorized owning Firm, when the caller selected one explicitly.
    /// The transaction refuses a brand that moved away from this Firm.
    pub firm_id: Option<Uuid>,
    /// The matter's display name. Required.
    pub name: String,
    /// The matter's code, stored exactly as supplied (after normalizing case
    /// and whitespace — see [`normalize_code`]). This *is* the stored code:
    /// [`open_matter`] does not generate or append anything to it. A code is
    /// chosen once and never changes — `project.code` is `READONLY` (see
    /// [`docs/glossary/project.md`](../../docs/glossary/project.md)) — and it
    /// is a coordinate shared with three systems Navigator does not own (a
    /// repository's `navigator.yaml`, the matter's Drive folder name, and its
    /// Notion `Project code` URL), so the caller — not Navigator — must own
    /// picking it. A collision with an already-open matter is refused with
    /// [`OpenMatterError::CodeConflict`] rather than silently resolved. The
    /// code must still pass [`is_valid_code`]: lowercase letters, digits, and
    /// single hyphens, alphanumeric at both ends, and not a reserved word.
    pub code: String,
    /// The client of record — a pre-existing `Role::Client` person, never a
    /// firm attorney (the firm-as-its-own-client default is a loyalty problem
    /// both councils flagged).
    pub client_id: Uuid,
    /// The pre-existing entity the matter opens against (`projects.entity_id`
    /// is NOT NULL).
    pub entity_id: Uuid,
    /// The matter's governing jurisdiction, if the opening caller has
    /// already resolved one. `None` preserves an unsettled matter.
    pub jurisdiction_id: Option<Uuid>,
    /// The matter's scope narrative.
    pub description: Option<String>,
    /// Which house brand's storefront this matter was opened through — see
    /// [`Project::brand`]. The caller resolves this from the request's
    /// `Host:` header (or, for a door with no request — the CLI, an MCP
    /// tool — the firm's own default brand); it is never read from form or
    /// JSON input the caller deserialized, so a spoofed field cannot reach
    /// here.
    pub brand: String,
    /// The opening attorney's conflict attestation. Required on **every** open
    /// — at this firm `lawyer` is an attorney, so a lawyer/admin session opening
    /// a Project is an attorney attesting they have checked for conflicts, and
    /// that either none prevent the open or the Project is not legal advice.
    /// A missing attestation is refused; it is never defaulted true.
    pub attestation: bool,
    /// The attesting attorney — the opening `lawyer` (=attorney) person, who
    /// becomes the matter's accountable lawyer DRI and the actor on the
    /// attestation audit row.
    pub acting_person_id: Uuid,
    /// Open the matter already closed — an engagement that ended before
    /// anyone opened its row (ENG-469). `None` is the ordinary open
    /// (`status = 'open'`, no `closed_at`); `Some` writes `status =
    /// 'closed'` and this timestamp in the same insert.
    ///
    /// This is deliberately **not** a create-then-[`transition_project`]
    /// sequence: that command refuses an `effective_at` before the row's
    /// own `inserted_at`, which a row [`open_matter`] is creating this
    /// instant would make impossible to satisfy for any genuinely
    /// historical close date. Baking the state into the initial row sidesteps
    /// that comparison entirely — there is no prior state to have preceded.
    /// The one bound that still applies is [`transition_project`]'s other
    /// rule, checked here directly: a close cannot be dated in the future.
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A matter could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenMatterError {
    /// The request is malformed in a way the caller can correct.
    #[error("{0}")]
    Invalid(&'static str),
    /// The named client of record is not a `Role::Client` person — a matter
    /// cannot be opened for a firm attorney as its own client.
    #[error("the client of record must be a client, not a firm attorney")]
    ClientNotAllowed,
    /// The attester (`acting_person_id`) is not a firm lawyer. At this firm the
    /// `lawyer`/`admin` tier is the attorney, and the attester becomes the
    /// accountable lawyer DRI, so a `client` or `clerk` can never attest or hold
    /// that DRI.
    #[error("the attesting attorney must be a firm lawyer")]
    AttesterNotAllowed,
    /// A referenced row (client, entity, or attester) does not exist.
    /// Carries which one so the adapter can point the caller at it.
    #[error("no such {0}")]
    NotFound(&'static str),
    /// No conflict attestation was supplied. Every Project open requires the
    /// opening attorney to attest; there is no default.
    #[error("a matter open requires the attorney's conflict attestation")]
    AttestationRequired,
    /// The conflict check found the matter adverse to a current client. This
    /// is a hard stop no attestation overrides; a waiver is a separate flow.
    #[error("conflict check blocked this matter: adverse to a current client")]
    BlockingConflict(Vec<String>),
    /// The supplied project code is already in use by another matter. The
    /// caller picks a different code; Navigator never resolves this itself.
    #[error("that project code is already in use")]
    CodeConflict,
    /// Appending the conflict attestation to the Relationship Log
    /// failed. The matter is already open at that point — the attestation
    /// is the last step — so this reports a matter whose audit entry is
    /// missing rather than a matter that did not open.
    #[error("record the conflict attestation: {0}")]
    Attestation(crate::relationship_logs::RelationshipLogError),
    /// A database failure, carrying the engine's own message.
    #[error("database: {0}")]
    Db(String),
}

/// Open a new matter. The single write behind the lawyer `POST /app/projects`
/// form, the CLI `project create`, and the JSON `POST /app/api/projects` command,
/// so every door runs the same conflict check, the same required attestation,
/// and writes the same audit trail — no door can open a matter without them.
///
/// The order is load-bearing. Cheap caller-correctable input checks run first;
/// then references, Firm ownership, and conflicts are checked. PostgreSQL commits
/// the Project and both DRI designations together. The conflict attestation is
/// recorded after that transaction; an audit failure is returned to the caller.
///
/// The attestation is required on **every** open and audited on **every**
/// open, not just flagged ones (see [`OpenMatterCommand::attestation`]). The
/// conflict findings, if any, are recorded in the audit `detail` alongside the
/// attestation, so the durable record shows what the attorney attested over.
/// Confirm the matter-open references resolve before any write: the client of
/// record exists and is a `Role::Client` (never a firm attorney), and the
/// entity, and attester exist. A missing reference names which one so
/// the adapter can point the caller at it.
///
/// Read one person's row for the two `role` checks the open depends on.
///
/// # This takes no lock, and that is a narrowing on the record
///
/// This is a plain read, so it cannot hold the row against a concurrent
/// admin `UPDATE` demoting the attester between the lawyer-tier check below
/// and the attestation write (navigator#790).
///
/// What holds the invariant is the door: a session carries the role it
/// authenticated with, and a role change forces re-authentication, so a
/// demoted attester cannot drive a *new* open. The residual window is one
/// already-in-flight open by a person demoted mid-request. Nick accepted
/// that narrowing on 2026-08-02.
async fn lock_person(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<Option<crate::persons::Person>, OpenMatterError> {
    crate::persons::find_by_id(application_store, person_id)
        .await
        .map_err(|err| person_lookup_failed(&err))
}

/// A failed person lookup, reported as the `String` this command already
/// carries.
fn person_lookup_failed(err: &crate::persons::PersonError) -> OpenMatterError {
    OpenMatterError::Db(format!("resolve a matter-open reference: {err}"))
}

/// Validate every reference the open depends on, and return the attester's
/// tier — the caller needs it to derive the lawyer role's own participation
/// word rather than writing a fixed one.
async fn validate_open_references(
    application_store: &ApplicationStore,
    input: &OpenMatterCommand,
) -> Result<Role, OpenMatterError> {
    let client = lock_person(application_store, input.client_id)
        .await?
        .ok_or(OpenMatterError::NotFound("client"))?;
    if client.role != Role::Client {
        return Err(OpenMatterError::ClientNotAllowed);
    }
    // The engine does not validate a `record<entity>` link, so the matter
    // a client opens against is read back rather than trusted.
    if crate::entities::find_by_id(application_store, input.entity_id)
        .await
        .map_err(|error| OpenMatterError::Db(error.to_string()))?
        .is_none()
    {
        return Err(OpenMatterError::NotFound("entity"));
    }
    let attester = lock_person(application_store, input.acting_person_id)
        .await?
        .ok_or(OpenMatterError::NotFound("attester"))?;
    // The attester becomes the accountable lawyer DRI and the actor on the
    // attestation audit row, so the shared command enforces the "lawyer is the
    // attorney" invariant here rather than trusting each adapter's own session
    // gate — a `client` or `clerk` can never be recorded as the attesting
    // attorney, whichever door (web form, CLI, or API) drove the open.
    if !attester.role.is_lawyer_tier() {
        return Err(OpenMatterError::AttesterNotAllowed);
    }
    if let Some(jurisdiction_id) = input.jurisdiction_id {
        if crate::jurisdictions::find_by_id(application_store, jurisdiction_id)
            .await
            .map_err(|error| OpenMatterError::Db(error.to_string()))?
            .is_none()
        {
            return Err(OpenMatterError::NotFound("jurisdiction"));
        }
    }
    Ok(attester.role)
}

/// Resolve the `status`/`closed_at` pair a new row's initial `CREATE` should
/// carry, from [`OpenMatterCommand::closed_at`]. `None` is the ordinary
/// open; `Some` is validated against the one bound that still applies to a
/// row with no prior state — a close cannot be dated in the future — and
/// rendered in the same format [`transition_project`] stores.
fn initial_status_and_closed_at(
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(&'static str, Option<String>), OpenMatterError> {
    let Some(closed_at) = closed_at else {
        return Ok(("open", None));
    };
    if closed_at > chrono::Utc::now() {
        return Err(OpenMatterError::Invalid(
            "closed_at cannot be in the future.",
        ));
    }
    Ok((
        "closed",
        Some(closed_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
    ))
}

/// The conflict attestation that belongs with every opened matter.
///
/// It names who attested and, when the check was not clear, the findings they
/// attested over.
async fn record_open_attestation(
    application_store: &ApplicationStore,
    project_id: Uuid,
    acting_person_id: Uuid,
    clear: bool,
    findings: &[String],
) -> Result<(), OpenMatterError> {
    let detail = if clear {
        "Conflict attestation at matter open. No conflicts found.".to_string()
    } else {
        format!(
            "Conflict attestation at matter open over these findings:\n{}",
            findings.join("\n"),
        )
    };
    crate::relationship_logs::record(
        application_store,
        &crate::relationship_logs::NewRelationshipLog {
            actor_person_id: Some(acting_person_id),
            subject_type: "project".to_string(),
            subject_id: project_id,
            action: "conflict_attestation".to_string(),
            detail,
            project_id: Some(project_id),
        },
    )
    .await
    .map_err(OpenMatterError::Attestation)?;
    Ok(())
}

/// Resolve ownership without dropping an explicitly authorized Firm.
async fn resolve_open_firm(
    application_store: &ApplicationStore,
    input: &OpenMatterCommand,
) -> Result<Uuid, OpenMatterError> {
    // Validate every caller's selected brand against live rows. The API uses
    // the request brand by default or an explicitly authorized Firm/brand pair.
    if !crate::firms::brand_key_exists(application_store, &input.brand)
        .await
        .map_err(|error| OpenMatterError::Db(error.to_string()))?
    {
        return Err(OpenMatterError::NotFound("brand"));
    }
    let firm_id = crate::firms::firm_id_for_brand_key(application_store, &input.brand)
        .await
        .map_err(|error| OpenMatterError::Db(error.to_string()))?;
    let firm_id = firm_id.ok_or(OpenMatterError::Invalid(BRAND_WORN_BY_NO_FIRM))?;

    if input.firm_id.is_some_and(|expected| expected != firm_id) {
        return Err(OpenMatterError::Invalid(BRAND_WORN_BY_ANOTHER_FIRM));
    }

    Ok(firm_id)
}

/// Commit the canonical Project and both DRI rows in PostgreSQL.
async fn open_matter_in_postgres(
    _application_store: &ApplicationStore,
    pg: &PostgresDb,
    open: &crate::postgres::projects::OpenMatter<'_>,
) -> Result<Project, OpenMatterError> {
    use crate::postgres::projects::ProjectError;
    Box::pin(crate::postgres::projects::open_on(pg, open))
        .await
        .map_err(|error| match error {
            ProjectError::CodeTaken => OpenMatterError::CodeConflict,
            ProjectError::FirmBrandChanged => OpenMatterError::Invalid(
                "Firm and brand changed while opening the matter; retry the request.",
            ),
            other => OpenMatterError::Db(other.to_string()),
        })
}

pub async fn open_matter(
    application_store: &ApplicationStore,
    input: &OpenMatterCommand,
) -> Result<Project, OpenMatterError> {
    open_matter_with_id(application_store, input, None).await
}

/// Resume only the Project ID allocated to this staged notice. The caller
/// serializes decisions on the notice before using this entry point.
pub(crate) async fn open_matter_for_intake(
    application_store: &ApplicationStore,
    input: &OpenMatterCommand,
    intake_id: Uuid,
) -> Result<Project, OpenMatterError> {
    open_matter_with_id(application_store, input, Some(intake_id)).await
}

#[expect(
    clippy::too_many_lines,
    reason = "one open applies the same validation and conflict attestation to a new Project or an intake retry"
)]
async fn open_matter_with_id(
    application_store: &ApplicationStore,
    input: &OpenMatterCommand,
    intake_id: Option<Uuid>,
) -> Result<Project, OpenMatterError> {
    // The attorney's attestation gates every open, checked first: no amount of
    // valid form data opens a Project the attorney hasn't attested to.
    if !input.attestation {
        return Err(OpenMatterError::AttestationRequired);
    }
    let name = input.name.trim();
    if name.is_empty() {
        return Err(OpenMatterError::Invalid("Name is required."));
    }
    // The code is required, not blank, and is stored exactly as supplied —
    // normalized for case and whitespace, never generated or appended to. It
    // is a coordinate the caller already committed to in a repository's
    // `navigator.yaml`, a Drive folder name, or a Notion URL, so Navigator
    // storing anything else would be unfixable once `project.code` (READONLY)
    // is written. A collision is refused below as `CodeConflict`, not
    // resolved by generation.
    let code = normalize_code(&input.code).ok_or(OpenMatterError::Invalid("Code is required."))?;
    if !is_valid_code(&code) {
        return Err(OpenMatterError::Invalid(
            "Project code must use lowercase letters, digits, and single hyphens; \
             it must start and end with a letter or digit.",
        ));
    }
    let firm_id = resolve_open_firm(application_store, input).await?;

    // Validate every reference before opening. The project and both
    // participations commit in the explicit SurrealDB transaction below;
    // transaction conflicts retry with jitter. `lock_person` remains the
    // accepted narrowing.
    let attester_participation =
        participation_for_role(validate_open_references(application_store, input).await?);
    // Conflict check, before any write. The relationship graph is advisory to
    // clear but authoritative to block: a confident adverse link to a current
    // client hard-stops the open, and no attestation overrides it (a waiver is
    // a separate, heavier flow). A clear or softly-flagged check proceeds on
    // the attorney's attestation, which the audit row below records.
    let conflict =
        crate::conflicts::check_new_matter(application_store, input.client_id, input.entity_id)
            .await
            .map_err(OpenMatterError::Db)?;
    if conflict.has_blocking() {
        return Err(OpenMatterError::BlockingConflict(conflict.summary_lines()));
    }
    if let Some(id) = intake_id {
        if let Some(project) = find_by_id(application_store, id)
            .await
            .map_err(|error| OpenMatterError::Db(error.to_string()))?
        {
            if project.code != code
                || project.firm_id != Some(firm_id)
                || project.brand != input.brand
                || project.entity_id != input.entity_id
            {
                return Err(OpenMatterError::CodeConflict);
            }
            let roles = participations_for_project(application_store, id)
                .await
                .map_err(|error| OpenMatterError::Db(error.to_string()))?;
            if !roles
                .iter()
                .any(|role| role.is_lawyer_dri && role.person_id == input.acting_person_id)
                || !roles
                    .iter()
                    .any(|role| role.is_client_dri && role.person_id == input.client_id)
            {
                return Err(OpenMatterError::CodeConflict);
            }
            let logs = crate::relationship_logs::for_project(application_store, id)
                .await
                .map_err(OpenMatterError::Attestation)?;
            if !logs.iter().any(|log| {
                log.action == "conflict_attestation"
                    && log.actor_person_id == Some(input.acting_person_id)
            }) {
                record_open_attestation(
                    application_store,
                    id,
                    input.acting_person_id,
                    conflict.is_clear(),
                    &conflict.summary_lines(),
                )
                .await?;
            }
            return Ok(project);
        }
    }
    let (status, closed_at) = initial_status_and_closed_at(input.closed_at)?;

    let project = NewProject {
        code,
        name: name.to_string(),
        status: status.to_string(),
        brand: input.brand.clone(),
        entity_id: input.entity_id,
        firm_id: Some(firm_id),
        jurisdiction_id: input.jurisdiction_id,
        description: crate::people_commands::none_if_blank(input.description.as_deref()),
    };
    let open = crate::postgres::projects::OpenMatter {
        id: intake_id,
        project: &project,
        closed_at: closed_at.as_deref(),
        lawyer: input.acting_person_id,
        lawyer_participation: attester_participation,
        client: input.client_id,
    };
    let created = {
        let pg = authority()
            .await
            .map_err(|error| OpenMatterError::Db(error.to_string()))?;
        Box::pin(open_matter_in_postgres(application_store, &pg, &open)).await?
    };
    // The attestation is the record that the conflict check was run and
    // cleared — the record the firm's `@cleared` discipline rests on. It
    // lands where the graph lives: under the AGE engine its PostgreSQL row
    // names the Project by foreign key, so the matter's PostgreSQL copy
    // lands first. A failure of either is the caller's error.
    record_open_attestation(
        application_store,
        created.id,
        input.acting_person_id,
        conflict.is_clear(),
        &conflict.summary_lines(),
    )
    .await?;
    Ok(created)
}

/// Navigator-prefixed Project codes may start directly with a working Notation.
#[must_use]
pub fn requires_onboarding(code: &str) -> bool {
    !code.starts_with("navigator")
}

/// Whether a template's declared `kind` makes its notation the engagement that
/// opens a matter — the same classifier the engagement-first gate uses. Keyed
/// off the declared kind, never the template `code`, so a retainer template
/// named otherwise still counts as the engagement.
#[must_use]
pub fn template_opens_a_matter(kind: Option<&str>) -> bool {
    kind.and_then(rules::kind::Kind::parse)
        .is_some_and(rules::kind::Kind::opens_a_matter)
}

/// Whether a template's declared `kind` makes its notation the letter that
/// closes a matter — the mirror of [`template_opens_a_matter`]. Keyed off the
/// declared kind, never the template `code`, so a bespoke offboarding letter
/// named otherwise still counts as the closing letter.
#[must_use]
pub fn template_closes_a_matter(kind: Option<&str>) -> bool {
    kind.and_then(rules::kind::Kind::parse)
        .is_some_and(rules::kind::Kind::closes_a_matter)
}

/// From a matter's onboarding/offboarding facts, the two lifecycle warning
/// flags: `missing_onboarding` (no matter-opening engagement on file) and
/// `missing_offboarding_letter` (a `closed` matter with no offboarding letter
/// on file).
#[must_use]
pub fn matter_flags(has_engagement: bool, status: &str, has_closing: bool) -> (bool, bool) {
    let missing_onboarding = !has_engagement;
    let missing_offboarding_letter = status == "closed" && !has_closing;
    (missing_onboarding, missing_offboarding_letter)
}

/// The displayed lifecycle derived from completed DocuSign onboarding evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatterLifecycle {
    /// Open Project whose code exempts it from onboarding.
    OnboardingOptional,
    /// Open, awaiting completed DocuSign onboarding.
    NeedsOnboarding,
    /// Open, with verified DocuSign onboarding.
    OnboardingOnFile,
    /// Closed.
    Closed,
}

impl MatterLifecycle {
    /// The CSS class this state renders as. `matter-lifecycle` carries the
    /// shared pill shape; the modifier carries the colour.
    #[must_use]
    pub fn class(self) -> &'static str {
        match self {
            MatterLifecycle::OnboardingOptional => "matter-lifecycle",
            MatterLifecycle::NeedsOnboarding => "matter-lifecycle matter-lifecycle--yellow",
            MatterLifecycle::OnboardingOnFile => "matter-lifecycle matter-lifecycle--green",
            MatterLifecycle::Closed => "matter-lifecycle matter-lifecycle--red",
        }
    }

    /// The displayed execution-gated lifecycle label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MatterLifecycle::OnboardingOptional => "open",
            MatterLifecycle::NeedsOnboarding => "pitch",
            MatterLifecycle::OnboardingOnFile => "active",
            MatterLifecycle::Closed => "closed",
        }
    }

    /// Explain the evidence behind the lifecycle label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            MatterLifecycle::OnboardingOptional => "Onboarding is optional for this Project.",
            MatterLifecycle::NeedsOnboarding => {
                "The onboarding DocuSign envelope UUID has not been verified as completed."
            }
            MatterLifecycle::OnboardingOnFile => {
                "The onboarding DocuSign envelope UUID is verified as completed."
            }
            MatterLifecycle::Closed => "This matter is closed.",
        }
    }
}

/// The lifecycle state a matter row renders, from its `status` and whether
/// it is missing verified DocuSign onboarding. `missing_offboarding_letter` is taken
/// but deliberately does not branch the result: a closed matter is
/// [`MatterLifecycle::Closed`] whether or not it still owes its offboarding
/// letter, and that gap keeps surfacing through the existing "no offboarding
/// letter" badge alongside this indicator, not folded into a fourth colour.
///
/// Deliberately an exhaustive match on `(status == "closed", missing_onboarding)`
/// so the offboarding-letter parameter's non-effect on the outcome is visible at
/// the call site, not just asserted in a doc comment.
#[must_use]
pub fn matter_lifecycle(
    status: &str,
    missing_onboarding: bool,
    missing_offboarding_letter: bool,
) -> MatterLifecycle {
    match (
        status == "closed",
        missing_onboarding,
        missing_offboarding_letter,
    ) {
        (true, _, _) => MatterLifecycle::Closed,
        (false, true, _) => MatterLifecycle::NeedsOnboarding,
        (false, false, _) => MatterLifecycle::OnboardingOnFile,
    }
}

/// Display an exempt open Project without claiming evidence of a signed onboarding.
#[must_use]
pub fn project_lifecycle(
    code: &str,
    status: &str,
    missing_onboarding: bool,
    missing_closing: bool,
) -> MatterLifecycle {
    if !requires_onboarding(code)
        && missing_onboarding
        && status != "closed"
        && status != "archived"
    {
        return MatterLifecycle::OnboardingOptional;
    }
    matter_lifecycle(status, missing_onboarding, missing_closing)
}

/// Onboarding is official when Navigator holds a completed DocuSign envelope UUID
/// on the onboarding notation or a server-verified signed document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStatus {
    /// The Project can proceed without a signed engagement.
    Optional,
    /// The matter is still a pitch: no onboarding envelope UUID has verified
    /// DocuSign completion. Includes a matter whose envelope was
    /// sent and is still outstanding, and one that was declined, voided, or
    /// expired — none of those is execution.
    Pitch,
    /// Navigator holds the onboarding envelope UUID and verified completion.
    Official,
}

impl OnboardingStatus {
    /// The visible text label, in the firm's own words — `pitch` is the same
    /// word [`MatterLifecycle::label`] uses for the unpapered stage, and
    /// `official` is the word this status exists to earn.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Optional => "optional",
            Self::Pitch => "pitch",
            Self::Official => "official",
        }
    }

    /// The hover/assistive title, spelling out what the status did and did
    /// not verify, so the short label never has to carry the caveat alone.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Optional => MatterLifecycle::OnboardingOptional.title(),
            Self::Pitch => {
                "This matter is still a pitch: its onboarding document has not completed a \
                 DocuSign envelope verified by Navigator."
            }
            Self::Official => {
                "Onboarding is official: Navigator holds the DocuSign envelope UUID and verified completion."
            }
        }
    }

    /// Derive the status from verified completed DocuSign evidence.
    #[must_use]
    pub fn for_project(code: &str, completed: bool) -> Self {
        if completed {
            Self::Official
        } else if requires_onboarding(code) {
            Self::Pitch
        } else {
            Self::Optional
        }
    }
}

/// Read the Project's verified DocuSign onboarding evidence from PostgreSQL.
///
/// # Errors
///
/// PostgreSQL configuration, application admission, and query failures are returned to the caller.
pub async fn onboarding_status(
    _application_store: &crate::application_store::ApplicationStore,
    project_id: Uuid,
    code: &str,
) -> Result<OnboardingStatus, String> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| error.to_string())?;
    let official = crate::onboarding::official_projects(&pg, &[project_id])
        .await
        .map_err(|error| error.to_string())?;
    Ok(OnboardingStatus::for_project(
        code,
        official.contains(&project_id),
    ))
}

/// The displayed lifecycle keeps closed and archived states and gates active on execution.
#[must_use]
pub fn execution_lifecycle_label(code: &str, status: &str, official: bool) -> &'static str {
    match status {
        "archived" => "archived",
        _ => project_lifecycle(code, status, !official, false).label(),
    }
}

/// Projects with verified completed onboarding, resolved by one PostgreSQL query.
///
/// # Errors
///
/// PostgreSQL configuration, application admission, and query failures are returned to the caller.
pub async fn onboarding_official_sets(
    _application_store: &crate::application_store::ApplicationStore,
    projects: &[Project],
) -> Result<std::collections::HashSet<Uuid>, String> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| error.to_string())?;
    let ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    crate::onboarding::official_projects(&pg, &ids)
        .await
        .map_err(|error| error.to_string())
}

/// The latest unexecuted onboarding e-signature request for each of `projects` that has one.
///
/// # Errors
///
/// PostgreSQL configuration, application admission, and query failures are returned to the caller.
pub async fn onboarding_esign_pending(
    _application_store: &crate::application_store::ApplicationStore,
    projects: &[Project],
) -> Result<std::collections::HashMap<Uuid, crate::onboarding::EsignSummary>, String> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| error.to_string())?;
    let ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    crate::onboarding::pending_esign(&pg, &ids)
        .await
        .map_err(|error| error.to_string())
}

/// Projects with filed opening and closing documents, read from PostgreSQL.
///
/// A classified Asset or a Notation with a produced review document establishes
/// filing. A questionnaire walk alone supplies no document. Execution status is
/// derived by [`onboarding_official_sets`].
///
/// # Errors
///
/// PostgreSQL configuration, application admission, and query failures are returned to the caller.
pub async fn matter_lifecycle_sets(
    _application_store: &crate::application_store::ApplicationStore,
    projects: &[Project],
) -> Result<
    (
        std::collections::HashSet<Uuid>,
        std::collections::HashSet<Uuid>,
    ),
    String,
> {
    let pg = crate::postgres::PostgresDb::application()
        .await
        .map_err(|error| error.to_string())?;
    let ids: Vec<_> = projects.iter().map(|project| project.id).collect();
    crate::onboarding::filed_documents(&pg, &ids)
        .await
        .map_err(|error| error.to_string())
}

/// One Brand's line in the nightly open-matters notice.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BrandOpenPitch {
    /// The Brand's display name.
    pub brand: String,
    /// Matters wearing the Brand whose status is not `closed`.
    pub open: usize,
    /// The open matters still pitches: the [`MatterLifecycle::NeedsOnboarding`]
    /// subset, no onboarding artifact on file yet.
    pub pitches: usize,
}

/// From a Firm's Brands as `(key, name)` and each matter's
/// `(brand, status, missing_onboarding)`, one [`BrandOpenPitch`] per Brand:
/// how many of its matters are open (`status != "closed"`) and how many of
/// those are still pitches. Every Brand gets a line, zero or not; a matter
/// wearing a Brand outside the list never counts. Lines sort by open count,
/// busiest first, then by name.
///
/// Pure and exposed so the nightly follow-up post's counts are unit-tested
/// without a database — see [`matter_open_pitch_counts`] for the
/// database-backed caller.
#[must_use]
pub fn open_pitch_counts(
    brands: &[(&str, &str)],
    rows: &[(&str, &str, bool)],
) -> Vec<BrandOpenPitch> {
    let mut lines: Vec<BrandOpenPitch> = brands
        .iter()
        .map(|(key, name)| {
            let (open, pitches) = rows
                .iter()
                .filter(|(brand, status, _)| brand == key && *status != "closed")
                .fold((0, 0), |(open, pitches), (_, _, missing_onboarding)| {
                    (open + 1, pitches + usize::from(*missing_onboarding))
                });
            BrandOpenPitch {
                brand: (*name).to_owned(),
                open,
                pitches,
            }
        })
        .collect();
    lines.sort_by(|left, right| {
        right
            .open
            .cmp(&left.open)
            .then_with(|| left.brand.cmp(&right.brand))
    });
    lines
}

/// The open/pitch counts of every Brand the Firm owns, for the nightly
/// follow-up post in that Firm's Slack, using the shared verified onboarding
/// evidence.
pub async fn matter_open_pitch_counts(
    application_store: &ApplicationStore,
    firm_id: Uuid,
) -> Result<Vec<BrandOpenPitch>, ProjectStoreError> {
    let brands = crate::brands::for_firm(application_store, firm_id)
        .await
        .map_err(|error| ProjectStoreError::Brand(Box::new(error)))?;
    let projects = all(application_store).await?;
    let has_engagement = onboarding_official_sets(application_store, &projects)
        .await
        .map_err(ProjectStoreError::Lifecycle)?;
    let brands: Vec<(&str, &str)> = brands
        .iter()
        .map(|brand| (brand.key.as_str(), brand.name.as_str()))
        .collect();
    let rows: Vec<(&str, &str, bool)> = projects
        .iter()
        .map(|project| {
            (
                project.brand.as_str(),
                project.status.as_str(),
                requires_onboarding(&project.code) && !has_engagement.contains(&project.id),
            )
        })
        .collect();
    Ok(open_pitch_counts(&brands, &rows))
}

#[cfg(test)]
mod surreal_read_tests {
    use super::open_pitch_counts;

    fn line(brand: &str, open: usize, pitches: usize) -> super::BrandOpenPitch {
        super::BrandOpenPitch {
            brand: brand.to_owned(),
            open,
            pitches,
        }
    }

    /// Every Brand the Firm owns gets a line, busiest first; closed matters
    /// count for nothing, and a quiet Brand still reads as zero.
    #[test]
    fn open_pitch_counts_give_every_firm_brand_a_line_busiest_first() {
        let brands = [
            ("neon", "Neon Law"),
            ("same-day", "Same Day Legal"),
            ("quiet", "Quiet Brand"),
        ];
        let rows = [
            ("neon", "open", true),
            ("neon", "closed", true),
            ("same-day", "open", true),
            ("same-day", "open", false),
            ("quiet", "closed", false),
        ];
        assert_eq!(
            open_pitch_counts(&brands, &rows),
            vec![
                line("Same Day Legal", 2, 1),
                line("Neon Law", 1, 1),
                line("Quiet Brand", 0, 0),
            ]
        );
    }

    /// A matter wearing another Firm's Brand never moves this Firm's numbers.
    #[test]
    fn open_pitch_counts_ignore_brands_outside_the_firm() {
        let rows = [("neon", "open", false), ("other-firm", "open", true)];
        assert_eq!(
            open_pitch_counts(&[("neon", "Neon Law")], &rows),
            vec![line("Neon Law", 1, 0)]
        );
        assert!(open_pitch_counts(&[], &rows).is_empty());
    }

    /// A repository URL may name any forge, in any organization.
    ///
    /// This is the whole point of storing a URL rather than composing one from
    /// a deployment-wide host: two Projects can legitimately live on different
    /// forges, so nothing here privileges one.
    #[test]
    fn a_repository_url_may_name_any_forge_and_any_organization() {
        for url in [
            "https://github.com/neon-law-source-code/navigator-sample-project",
            "https://gitlab.com/some-group/some-subgroup/a-project",
            "https://git.example.internal/an-org/a-project.git",
            // A self-hosted forge on a port, and a plain-http intranet remote.
            "https://forge.example:8443/an-org/a-project",
            "http://forge.internal/an-org/a-project",
        ] {
            assert!(
                super::is_valid_repository_url(url),
                "{url} must be accepted — any forge, any organization"
            );
        }
    }

    /// The shapes that are refused, and why each one matters.
    #[test]
    fn a_repository_url_refuses_unclonable_and_unsafe_values() {
        for (url, why) in [
            ("", "blank is not a URL"),
            ("   ", "whitespace is not a URL"),
            ("github.com/an-org/a-project", "no scheme"),
            ("ssh://git@forge.example/an-org/a", "ssh is not http(s)"),
            ("file:///etc/passwd", "file:// would read the serving host"),
            (
                "javascript:alert(1)//x/y",
                "a live-link scheme must never render",
            ),
            ("https://github.com", "a forge root is not a repository"),
            ("https://github.com/", "an empty path is not a repository"),
            (
                "https://user:token@forge.example/an-org/a",
                "an embedded credential must not enter a rendered, logged column",
            ),
            (
                "https://forge.example/an org/a",
                "whitespace cannot survive into a clone argument",
            ),
        ] {
            assert!(
                !super::is_valid_repository_url(url),
                "{url:?} must be refused: {why}"
            );
        }
    }
}

#[cfg(test)]
mod drive_folder_input_tests {
    use super::drive_folder_id_from_input;

    /// Every link shape Drive hands out for a folder — a My Drive folder and a
    /// shared-drive folder look the same — and a bare id resolve to the id.
    #[test]
    fn every_drive_folder_link_shape_and_a_bare_id_resolve_to_the_id() {
        for input in [
            "1SyntheticFolderId_-0",
            "  1SyntheticFolderId_-0  ",
            "https://drive.google.com/drive/folders/1SyntheticFolderId_-0",
            "https://drive.google.com/drive/folders/1SyntheticFolderId_-0?usp=sharing",
            "https://drive.google.com/drive/folders/1SyntheticFolderId_-0/",
            "https://drive.google.com/drive/u/0/folders/1SyntheticFolderId_-0",
            "https://drive.google.com/drive/u/2/folders/1SyntheticFolderId_-0?usp=drive_link",
            "https://drive.google.com/open?id=1SyntheticFolderId_-0",
            "https://drive.google.com/folderview?id=1SyntheticFolderId_-0&usp=sharing",
        ] {
            assert_eq!(
                drive_folder_id_from_input(input).as_deref(),
                Some("1SyntheticFolderId_-0"),
                "{input}"
            );
        }
    }

    /// Anything that is not a Drive folder link or an id is refused rather
    /// than half-parsed: another host, a file link, a missing id, an id that
    /// could change an API path.
    #[test]
    fn anything_else_is_refused() {
        for input in [
            "",
            "not an id",
            "https://drive.example.com/drive/folders/1SyntheticFolderId",
            "https://drive.google.com/file/d/1SyntheticFileId/view",
            "https://drive.google.com/drive/folders/",
            "https://drive.google.com/drive/my-drive",
            "https://drive.google.com/open?usp=sharing",
            "https://drive.google.com/drive/folders/1Synthetic%2F..%2Fid",
            "javascript:alert(1)",
        ] {
            assert_eq!(drive_folder_id_from_input(input), None, "{input}");
        }
    }
}

#[cfg(test)]
mod kit_version_tests {
    use super::is_valid_kit_version;

    /// A recorded Kit version is `{jurisdiction}/{practice_area}/{version}` —
    /// the shape `navigator project skill use` writes through.
    #[test]
    fn a_three_segment_kit_version_is_valid() {
        for value in [
            "NV/estates/1",
            "ny/uncontested-divorce/1.0.0",
            "TX/probate/2026.01",
        ] {
            assert!(is_valid_kit_version(value), "{value}");
        }
    }

    /// Anything that is not three non-empty, whitespace-free segments is
    /// refused rather than normalized — a mis-shaped value on the matter row
    /// would defeat the report it exists for.
    #[test]
    fn anything_else_is_refused() {
        for value in [
            "",
            "NV/estates",
            "NV/estates/1/two",
            "NV/estates/",
            "/estates/1",
            "NV/uncontested divorce/1",
            " NV/estates/1",
        ] {
            assert!(!is_valid_kit_version(value), "{value}");
        }
    }
}

#[cfg(test)]
mod project_code_resolution_tests {}

#[cfg(test)]
mod project_jurisdiction_tests {}

#[cfg(test)]
mod onboarding_status_tests {
    #[test]
    fn active_requires_official_execution() {
        assert_eq!(
            super::execution_lifecycle_label("navigator-acme", "open", false),
            "open"
        );
        assert_eq!(
            super::OnboardingStatus::for_project("navigator", false),
            super::OnboardingStatus::Optional
        );
        assert_eq!(
            super::OnboardingStatus::for_project("navigator", true),
            super::OnboardingStatus::Official
        );
        assert_eq!(
            super::execution_lifecycle_label("matter", "open", false),
            "pitch"
        );
        assert_eq!(
            super::execution_lifecycle_label("matter", "open", true),
            "active"
        );
        assert_eq!(
            super::execution_lifecycle_label("matter", "closed", false),
            "closed"
        );
        assert_eq!(
            super::execution_lifecycle_label("matter", "archived", true),
            "archived"
        );
    }
    #[test]
    fn a_random_code_is_a_valid_unique_hex_code() {
        let first = super::random_code();
        assert_eq!(first.len(), 16, "{first}");
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{first}"
        );
        assert!(super::is_valid_code(&first), "{first}");
        assert_ne!(
            first,
            super::random_code(),
            "each call draws fresh randomness"
        );
    }
}

/// Matter-open, delete, and archive-restore commands — the write paths the
/// SurrealDB store owns outright. `unmigrated()` + `apply()` follows this
/// file's existing read-test convention; no new embedded-engine call sites
/// (`unmigrated` is not a ratchet-counted symbol).
#[cfg(test)]
mod matter_command_tests {}
