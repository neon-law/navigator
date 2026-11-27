//! Walk a Notation's questionnaire one answer at a time.
//!
//! Both the admin HTML form (`portal::retainer_walk`) and the MCP
//! tools (`create_notation`, `answer_notation`) drive a
//! Notation through the same two state machines: a questionnaire
//! that asks one question per signal, then a post-intake workflow.
//! This module owns the questionnaire half; the workflow half is
//! driven by `portal::retainer_walk::begin_post_questionnaire_workflow`,
//! which advances completed intake to its declared human review gate.
//!
//! The runtime — not the application — is the source of truth for
//! questionnaire state. That mirrors `retainer_walk` exactly: in
//! production, the `workflows-service` worker journals each
//! transition inside `ctx.run("append-…", …)`; in tests, the
//! in-memory runtime records the transition in its own `Vec`.
//! Callers do not write `notation_events` themselves.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cloud::StorageService;
use thiserror::Error;
use uuid::Uuid;

use crate::runtime::{SignalContext, StateMachineRuntime, WorkflowRuntimeError};
use crate::spec::{
    metadata_keys_for_state, question_code_for_state, MachineKind, QuestionnaireSpec, StateName,
    WorkflowSpecError,
};
use crate::specs::{
    audiences_from_template, audiences_from_yaml, catalog_spec_yaml, choices_from_template,
    choices_from_yaml, prompt_overrides_from_template, prompt_overrides_from_yaml,
    questionnaire_spec_from_template, questionnaire_spec_from_yaml, template_has_questionnaire,
};

/// One question presented to the caller — the prompt, the answer
/// shape, and the stable code the caller must echo back on the
/// next `answer_step`. `id` is the row id of the question; the
/// MCP surface ignores it but the admin HTML form uses it to look
/// up any prior answer for the (question, person) pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionDescriptor {
    pub id: Uuid,
    pub code: String,
    pub prompt: String,
    pub answer_type: String,
    pub choices: Vec<QuestionChoice>,
    /// The attorney-authored guidance for this question, read from its
    /// canonical `Question.yaml` definition. `None` when the question
    /// declares none. Like `choices` it has no column on the `question`
    /// table, so it comes from the seed bytes rather than the row.
    ///
    /// Not every seeded string is written for the respondent: the four
    /// [`AUTHOR_FACING_HELP_CODES`] address the template author, so a
    /// [`client_intake_step`] descriptor carries `None` for them while the
    /// lawyer walk keeps the string. A missing client lede on a `custom_*`
    /// step is that choice, not a dropped value.
    pub help_text: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct QuestionnaireDefinition {
    spec: QuestionnaireSpec,
    prompts: BTreeMap<String, String>,
    #[serde(default)]
    audiences: BTreeMap<String, String>,
    #[serde(default)]
    choices: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuestionChoice {
    pub value: String,
    pub label: String,
}

/// Where the questionnaire is after a `start_notation` /
/// `answer_step` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextStep {
    /// The caller must collect this answer and call `answer_step`.
    NeedsAnswer { question: QuestionDescriptor },
    /// The questionnaire reached `END`. The post-intake workflow
    /// has *not* been started by this module — the caller decides
    /// when and how to kick it off.
    QuestionnaireComplete,
}

/// Output of [`start_notation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartOutcome {
    pub notation_id: Uuid,
    pub next: NextStep,
}

/// Who entered an answer. The notation's bound Person is always the
/// *respondent* (`answers.person_id`); this records who actually typed
/// the value — the lawyer filling it in on the client's behalf, or
/// the client themselves through the magic link — and the authorship
/// `source` ([`answer::SOURCE_LAWYER`] / [`answer::SOURCE_CLIENT`]) that
/// the data lake groups by.
#[derive(Debug, Clone, Copy)]
pub struct AnswerAuthor<'a> {
    /// FK → persons: who typed the answer. `None` for system/agent
    /// answers with no individual Person row.
    pub authored_by: Option<Uuid>,
    /// `answer::SOURCE_LAWYER` or `answer::SOURCE_CLIENT`.
    pub source: &'a str,
}

impl AnswerAuthor<'static> {
    /// A lawyer-sourced answer typed by `authored_by` (the logged-in
    /// lawyer/admin person, or `None` for the agent surface).
    #[must_use]
    pub fn lawyer(authored_by: Option<Uuid>) -> Self {
        Self {
            authored_by,
            source: store::answers::SOURCE_LAWYER,
        }
    }

    /// A client-sourced answer self-entered by `authored_by` through the
    /// magic link.
    #[must_use]
    pub fn client(authored_by: Option<Uuid>) -> Self {
        Self {
            authored_by,
            source: store::answers::SOURCE_CLIENT,
        }
    }

    /// A machine-extracted answer proposed by batch transcript coverage. It
    /// belongs to no individual typist (`authored_by = None`) and is written
    /// [`answer::SOURCE_EXTRACTED`] so the walk surfaces it as a default the
    /// lawyer confirms or edits — never a silently-accepted answer.
    #[must_use]
    pub fn extracted() -> Self {
        Self {
            authored_by: None,
            source: store::answers::SOURCE_EXTRACTED,
        }
    }
}

#[derive(Debug, Error)]
pub enum NotationSessionError {
    #[error("template `{0}` not found")]
    TemplateNotFound(String),
    #[error(
        "the first notation on a matter must be the engagement that opens it — a retainer or an \
         onboarding — not `{code}` (kind: {kind})"
    )]
    EngagementMustBeFirst { code: String, kind: String },
    #[error("template `{0}` has no questionnaire frontmatter")]
    TemplateHasNoQuestionnaire(String),
    #[error(
        "template body storage is required to resolve a notation without a frozen questionnaire"
    )]
    QuestionnaireStorageRequired,
    #[error("the completed questionnaire definition is unavailable; begin a fresh intake")]
    QuestionnaireAuthorityMissing,
    #[error("reading pinned template body: {0}")]
    TemplateBody(#[from] store::templates::TemplateBodyError),
    #[error("reading template from the project repo: {0}")]
    TemplateSource(#[from] store::template_source::TemplateSourceError),
    #[error("template: {0}")]
    Template(#[from] store::templates::TemplateError),
    #[error("reading template from the Firm Library: {0}")]
    FirmLibrary(Box<store::firm_library::FirmLibraryError>),
    #[error("notation `{0}` not found")]
    NotationNotFound(Uuid),
    #[error("question `{0}` not seeded in store")]
    QuestionNotSeeded(String),
    #[error("question `{0}` is not a client-facing question on this notation's intake")]
    QuestionNotClientFacing(String),
    #[error("question `{0}` was not flagged for re-collection by the lawyer review")]
    QuestionNotFlagged(String),
    #[error("re-collect only questions on the completed questionnaire route")]
    QuestionNotOnRoute,
    #[error("this correction changes the questionnaire route; begin a fresh intake")]
    QuestionnaireRouteChanged,
    #[error("question code mismatch: questionnaire is currently asking `{expected}`, got `{got}`")]
    QuestionMismatch { expected: String, got: String },
    /// An answer named an option its question never declared.
    ///
    /// Carries the state and the declared option keys — both firm-authored
    /// template metadata — and deliberately **not** the submitted value,
    /// which is a client answer. This error is surfaced to callers and
    /// logged, and answers are client content that never reaches a log line.
    #[error("`{state}` accepts only these options: {}", declared.join(", "))]
    UndeclaredChoice {
        state: String,
        declared: Vec<String>,
    },
    #[error("questionnaire is already complete")]
    AlreadyComplete,
    /// A recorded answer routes nowhere — no choice edge matches and
    /// no `_` fallback exists. The route gate refuses to compile such
    /// a template (uncovered choice), so reaching this at runtime
    /// means a walk is running against a snapshot the gate never
    /// saw — the defense-in-depth arm, never the happy path.
    #[error(
        "questionnaire answer at `{state}` routes nowhere — no choice edge matches and no `_` \
         fallback exists"
    )]
    RouteDeadEnd { state: String },
    #[error("workflow runtime: {0}")]
    Runtime(#[from] WorkflowRuntimeError),
    #[error("database: {0}")]
    Db(String),
    #[error("notation store: {0}")]
    Notation(#[from] store::notations::NotationError),
    /// The questionnaire's questions and answers moved to SurrealDB with
    /// wave five (ENG-121); the notation and its template, then the
    /// `notation_event` / `reask` journal tables, followed with later
    /// slices of the same wave, so a session now fails against Surreal
    /// throughout.
    #[error("question store: {0}")]
    Question(#[from] store::questions::QuestionError),
    #[error("reask journal: {0}")]
    Reask(#[from] store::reask::ReaskError),
    #[error("answer store: {0}")]
    Answer(#[from] store::answers::AnswerError),
    /// LAW-57: a client-facing address-shaped question falls back to the
    /// respondent's stored [`store::addresses::Address`] when no answer
    /// exists yet — see [`address_prior_value`].
    #[error("address store: {0}")]
    Address(#[from] store::addresses::AddressError),
    #[error("spec parse: {0}")]
    Spec(#[from] WorkflowSpecError),
    #[error("encoding questionnaire snapshot: {0}")]
    SnapshotEncode(String),
    #[error("decoding questionnaire snapshot: {0}")]
    SnapshotDecode(String),
}

impl From<String> for NotationSessionError {
    fn from(message: String) -> Self {
        Self::Db(message)
    }
}

impl NotationSessionError {
    /// A fault the lawyer can fix in the template or the matter.
    ///
    /// The notation-create door turns this into a 422 whose body is the
    /// message. A store failure — a project read, a question write, or a
    /// template-row write — stays [`None`], because those include database
    /// failures and the Draft may be valid. The durable runtime and a missing
    /// person row stay internal the same way.
    #[must_use]
    pub fn lawyer_fault(&self) -> Option<String> {
        match self {
            Self::TemplateHasNoQuestionnaire(code) => Some(format!(
                "template `{code}` has no questionnaire frontmatter"
            )),
            Self::QuestionNotSeeded(code) => Some(format!(
                "question `{code}` is not seeded; the template names a question the store does not have"
            )),
            Self::RouteDeadEnd { state } => Some(format!(
                "questionnaire answer at `{state}` routes nowhere — no choice edge matches and no `_` fallback exists"
            )),
            Self::UndeclaredChoice { state, declared } => Some(format!(
                "`{state}` accepts only these options: {}",
                declared.join(", ")
            )),
            Self::SnapshotEncode(message) | Self::SnapshotDecode(message) => {
                Some(format!("questionnaire snapshot: {message}"))
            }
            _ => None,
        }
    }
}

/// Create a fresh Notation for `template_code`, start the
/// questionnaire runtime, and return the first question.
///
/// `person_id`, `project_id`, and `entity_id` are caller-resolved —
/// neither this module nor the runtime invents respondents,
/// matters, or entities. Every Notation belongs to exactly one
/// Project; see `docs/notation.md#notation`.
#[allow(clippy::too_many_arguments)]
pub async fn start_notation(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    template_code: &str,
    person_id: Uuid,
    project_id: Uuid,
    entity_id: Option<Uuid>,
) -> Result<StartOutcome, NotationSessionError> {
    start_notation_with_delivery(
        application_store,
        runtime,
        storage,
        template_code,
        person_id,
        project_id,
        entity_id,
        store::notations::SigningDelivery::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn start_notation_with_delivery(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    template_code: &str,
    person_id: Uuid,
    project_id: Uuid,
    entity_id: Option<Uuid>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    // Prefer a Project-scoped template, falling back to the shared one.
    let template_row =
        store::templates::resolve(application_store, Some(project_id), template_code)
            .await?
            .ok_or_else(|| NotationSessionError::TemplateNotFound(template_code.into()))?;

    start_resolved_notation(
        application_store,
        runtime,
        storage,
        StartPinnedNotation {
            template: &template_row,
            person_id,
            project_id,
            entity_id,
        },
        delivery,
    )
    .await
}

/// Named coordinates for a Project-scoped immutable preview version.
pub struct StartPinnedNotation<'a> {
    pub template: &'a store::templates::Template,
    pub person_id: Uuid,
    pub project_id: Uuid,
    pub entity_id: Option<Uuid>,
}

/// Start from the exact persisted Project version returned by the authoring write.
/// Shared and Firm Library resolution remains with the existing code-resolving entry point.
/// # Errors
/// Refuses another scope or Project, and propagates questionnaire/runtime/store failures.
pub async fn start_notation_pinned(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    request: StartPinnedNotation<'_>,
) -> Result<StartOutcome, NotationSessionError> {
    start_notation_pinned_with_delivery(
        application_store,
        runtime,
        storage,
        request,
        store::notations::SigningDelivery::default(),
    )
    .await
}

/// Start the selected Project version with the specified signing delivery.
pub async fn start_notation_pinned_with_delivery(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    request: StartPinnedNotation<'_>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    if request.template.project_id != Some(request.project_id) || request.template.firm_id.is_some()
    {
        return Err(NotationSessionError::TemplateNotFound(
            request.template.code.clone(),
        ));
    }
    start_resolved_notation(application_store, runtime, storage, request, delivery).await
}

async fn start_resolved_notation(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    request: StartPinnedNotation<'_>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    let StartPinnedNotation {
        template: template_row,
        person_id,
        project_id,
        entity_id,
    } = request;
    let definition = questionnaire_definition_for(application_store, storage, template_row).await?;
    let snapshot = questionnaire_snapshot_from_definition(&definition)?;

    let mut new_notation = store::notations::NewNotation::new(
        template_row.id,
        person_id,
        project_id,
        StateName::BEGIN,
    )
    .with_questionnaire_snapshot(snapshot)
    .with_delivery(delivery.as_str());
    if let Some(entity_id) = entity_id {
        new_notation = new_notation.with_entity(entity_id);
    }
    let notation_id = store::notations::create(application_store, &new_notation)
        .await?
        .id;

    runtime
        .start(
            MachineKind::Questionnaire,
            notation_id,
            definition.spec.inner(),
        )
        .await?;
    let next = first_step(application_store, &definition, notation_id).await?;
    Ok(StartOutcome { notation_id, next })
}

/// After a Project repository miss, persist `template_code` from the Firm
/// Library of the Firm that owns the Project, so [`store::templates::resolve`]
/// finds the Firm version before the shared catalog. A Project with no Firm,
/// a Firm with no library, or a library without the code is a quiet miss; a
/// Draft that IS in the library but fails validation refuses, exactly as an
/// invalid Project Draft does.
async fn persist_from_firm_library(
    application_store: &store::application_store::ApplicationStore,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    project_id: Uuid,
    template_code: &str,
) -> Result<(), NotationSessionError> {
    let firm_id = store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| NotationSessionError::Db(error.to_string()))?
        .and_then(|project| project.firm_id);
    let Some(firm_id) = firm_id else {
        return Ok(());
    };
    match store::firm_library::persist_draft(
        application_store,
        storage,
        repo,
        firm_id,
        template_code,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(store::firm_library::FirmLibraryError::Source(source)) => Err(source.into()),
        Err(other) => Err(NotationSessionError::FirmLibrary(Box::new(other))),
    }
}

/// The engagement-first gate shared by every door that opens a real
/// notation run: the first notation on a matter must normally be the
/// engagement that makes it official — a retainer, or the intake-driven
/// onboarding that opens a bundle of instruments. A filed onboarding document
/// also establishes the engagement. An open Project with no
/// notations may instead start with an offboarding letter so a pitch can
/// be closed.
///
/// Only gates a template that actually resolves — a code present in
/// neither the repo nor the catalog falls through so `start_notation`
/// surfaces `TemplateNotFound` (not a misleading engagement-first
/// error). Which kinds open a matter is `rules::kind`'s call, not
/// this module's: `opens_a_matter` is an exhaustive match, so a new
/// `Kind` must declare its side of the line, and the lawyer matter
/// list (`portal::admin::template_opens_a_matter`) reads the very same
/// classifier — one answer, two callers, no drift.
pub async fn enforce_engagement_first(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    template_code: &str,
) -> Result<(), NotationSessionError> {
    if store::notations::exists_for_project(application_store, project_id).await? {
        return Ok(());
    }
    let Some(template) =
        store::templates::resolve(application_store, Some(project_id), template_code).await?
    else {
        return Ok(());
    };
    enforce_engagement_first_resolved(application_store, project_id, &template).await
}

/// Apply the existing engagement rule to the exact version the caller will open.
/// # Errors
/// The same engagement-first or store failure as the code-resolving wrapper.
pub async fn enforce_engagement_first_pinned(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    template: &store::templates::Template,
) -> Result<(), NotationSessionError> {
    if template.project_id != Some(project_id) || template.firm_id.is_some() {
        return Err(NotationSessionError::TemplateNotFound(
            template.code.clone(),
        ));
    }
    enforce_engagement_first_resolved(application_store, project_id, template).await
}

async fn enforce_engagement_first_resolved(
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
    template: &store::templates::Template,
) -> Result<(), NotationSessionError> {
    if store::notations::exists_for_project(application_store, project_id).await? {
        return Ok(());
    }
    let kind = template.kind.as_deref().and_then(rules::kind::Kind::parse);
    let opens = kind.is_some_and(rules::kind::Kind::opens_a_matter);
    // A pitch has no opening notation by definition. It still needs
    // an offboarding document before it can close, so let its closing
    // letter be the first notation without weakening the engagement-
    // first rule for other kinds or Project lifecycle states.
    if opens {
        return Ok(());
    }
    if store::projects::find_by_id(application_store, project_id)
        .await
        .map_err(|error| NotationSessionError::Db(error.to_string()))?
        .is_some_and(|project| {
            !store::projects::requires_onboarding(&project.code)
                || (kind == Some(rules::kind::Kind::Offboarding) && project.status == "open")
        })
    {
        return Ok(());
    }
    if store::assets::kinds_by_projects(application_store, &[project_id])
        .await
        .map_err(|error| NotationSessionError::Db(error.to_string()))?
        .iter()
        .any(|(_, kind)| store::projects::template_opens_a_matter(kind.as_deref()))
    {
        return Ok(());
    }
    Err(NotationSessionError::EngagementMustBeFirst {
        code: template.code.clone(),
        kind: template.kind.clone().unwrap_or_else(|| "none".into()),
    })
}

/// The one `notation create` front door: auto-save the template from the
/// Project's git repo, then open the notation pinned to that exact version.
///
/// It runs the shared `read-repo → validate → persist` engine
/// ([`store::template_source::persist_from_repo`]) — reading
/// the chosen Project blueprint directory from repo HEAD, refusing loudly on any invalid
/// template, and appending an immutable project-scoped version pinned to
/// `(commit SHA + content hash)` — and then hands off to [`start_notation`],
/// which resolves that just-written project-scoped version (preferred over
/// the shared catalog, IsCurrent) and freezes its questionnaire. `web` and
/// `cli` both reach the engine through here, so the two surfaces open a
/// notation the same way; there is no separate `import` step.
#[allow(clippy::too_many_arguments)]
pub async fn create_notation_from_repo(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: &Arc<dyn StorageService>,
    repo: &repos::RepoStore,
    template_code: &str,
    person_id: Uuid,
    project_id: Uuid,
    entity_id: Option<Uuid>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    // Prefer a template authored in the Project's own repo — auto-save that
    // exact version and open pinned to it. When the code is not in the repo
    // (or the repo has no commits yet), fall back to the bundled firm
    // catalog: `start_notation` resolves the shared template line, so a
    // standard blueprint (a retainer, an LLC formation) opens on a fresh
    // matter with no per-project authoring. A template that IS in the repo
    // but fails validation still hard-errors — bad paper never opens.
    use store::template_source::TemplateSourceError;
    if let Err(e) = store::template_source::persist_from_repo(
        application_store,
        storage,
        repo,
        project_id,
        template_code,
    )
    .await
    {
        match e {
            // Not authored in the repo (or the repo has no commits yet) —
            // try the owning Firm's Library, then the bundled catalog below.
            TemplateSourceError::TemplateNotFound { .. } | TemplateSourceError::RepoEmpty(_) => {
                persist_from_firm_library(
                    application_store,
                    storage,
                    repo,
                    project_id,
                    template_code,
                )
                .await?;
            }
            // A template that IS in the repo but is invalid hard-errors.
            other => return Err(other.into()),
        }
    }
    open_persisted(
        application_store,
        runtime,
        storage,
        template_code,
        person_id,
        project_id,
        entity_id,
        delivery,
    )
    .await
}

/// A Draft the caller read from its Project checkout at a published commit.
#[derive(Debug, Clone)]
pub struct CheckoutDraft {
    pub contents: String,
    pub commit_sha: String,
}

/// [`create_notation_from_repo`] for a Draft the caller read from its own
/// checkout. The hosted repository store may hold no copy of a Project whose
/// repository lives on the forge, and a miss there opens the shared
/// catalog's template of the same code instead.
///
/// # Errors
/// As [`create_notation_from_repo`]; an invalid Draft refuses.
#[allow(clippy::too_many_arguments)]
pub async fn create_notation_from_checkout(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: &Arc<dyn StorageService>,
    draft: CheckoutDraft,
    template_code: &str,
    person_id: Uuid,
    project_id: Uuid,
    entity_id: Option<Uuid>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    store::template_source::persist_contents(
        application_store,
        storage,
        project_id,
        template_code,
        draft.contents,
        draft.commit_sha,
    )
    .await?;
    open_persisted(
        application_store,
        runtime,
        storage,
        template_code,
        person_id,
        project_id,
        entity_id,
        delivery,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn open_persisted(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: &Arc<dyn StorageService>,
    template_code: &str,
    person_id: Uuid,
    project_id: Uuid,
    entity_id: Option<Uuid>,
    delivery: store::notations::SigningDelivery,
) -> Result<StartOutcome, NotationSessionError> {
    // The first notation opened on a matter must normally be the engagement
    // that makes it official. The preview-push door enforces the same rule
    // (see [`enforce_engagement_first`]); the decision lives here, once.
    enforce_engagement_first(application_store, project_id, template_code).await?;

    start_notation_with_delivery(
        application_store,
        runtime,
        Some(storage),
        template_code,
        person_id,
        project_id,
        entity_id,
        delivery,
    )
    .await
}

/// Encode the questionnaire graph for a Template row so callers that
/// create Notations directly still freeze the same askable set as
/// [`start_notation`].
pub async fn questionnaire_snapshot_for_template(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    template_row: &store::templates::Template,
) -> Result<serde_json::Value, NotationSessionError> {
    let definition = questionnaire_definition_for(application_store, storage, template_row).await?;
    questionnaire_snapshot_from_definition(&definition)
}

/// The active walker's projected question order from its frozen questionnaire
/// and latest answers, including extracted defaults. Review and Re-ask use
/// [`questionnaire_chain_for_reask`] for the confirmed completed route.
pub async fn questionnaire_chain_for_notation(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<Vec<String>, NotationSessionError> {
    let (_, definition) = load_notation_and_spec(application_store, storage, notation_id).await?;
    let taken = recorded_answers(application_store, notation_id).await?;
    Ok(definition
        .spec
        .question_order(&taken)
        .into_iter()
        .map(|state| state.0)
        .collect())
}

enum RenderQuestionnaire {
    Absent {
        choices: BTreeMap<String, BTreeMap<String, String>>,
    },
    Present {
        definition: QuestionnaireDefinition,
        route: BTreeSet<String>,
    },
}

/// One answer projection for a document render. The route and eligible rows
/// come from the same answer read; this does not change the stored evidence.
pub struct RenderAnswerProjection {
    pub notation: store::notations::Notation,
    pub answers: Vec<store::answers::Answer>,
    pub codes: BTreeMap<Uuid, String>,
    questionnaire: RenderQuestionnaire,
}

impl RenderAnswerProjection {
    /// Whether a typed binding belongs to this route. A verified static
    /// document without a questionnaire retains its bound-record bindings.
    #[must_use]
    pub fn includes_state(&self, state: &str) -> bool {
        match &self.questionnaire {
            RenderQuestionnaire::Absent { .. } => true,
            RenderQuestionnaire::Present { route, .. } => route.contains(state),
        }
    }

    #[must_use]
    pub fn declares_state(&self, state: &str) -> bool {
        matches!(&self.questionnaire, RenderQuestionnaire::Present { definition, .. }
            if definition.spec.inner().states.keys().any(|key| key.as_str() == state))
    }

    #[must_use]
    pub fn choices(&self) -> &BTreeMap<String, BTreeMap<String, String>> {
        match &self.questionnaire {
            RenderQuestionnaire::Absent { choices } => choices,
            RenderQuestionnaire::Present { definition, .. } => &definition.choices,
        }
    }
}

/// Project body answers onto the verified frozen or pinned questionnaire route. Active intake
/// keeps proposed defaults; after the workflow leaves BEGIN, only confirmed
/// answers select the route and fill the body. No-questionnaire documents
/// retain their bindings only after their pinned body proves that absence.
///
/// # Errors
/// Malformed snapshots, unavailable or invalid pinned bodies, and store reads fail closed.
pub async fn render_answer_projection(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<RenderAnswerProjection, NotationSessionError> {
    answer_projection(application_store, storage, notation_id, false).await
}

async fn answer_projection(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    for_reask: bool,
) -> Result<RenderAnswerProjection, NotationSessionError> {
    let (notation, definition) =
        verified_questionnaire(application_store, storage, notation_id).await?;
    let mut answers = store::answers::for_notation(application_store, notation_id).await?;
    let ids: Vec<Uuid> = answers.iter().map(|answer| answer.question_id).collect();
    let codes: BTreeMap<Uuid, String> = store::questions::find_by_ids(application_store, &ids)
        .await?
        .into_iter()
        .map(|question| (question.id, question.code))
        .collect();
    let questionnaire = match definition {
        None => {
            // Static documents retain their established label formatting;
            // a catalog label never supplies questionnaire route authority.
            let choices = store::templates::find_by_id(application_store, notation.template_id)
                .await?
                .and_then(|template| catalog_spec_yaml(&template.code))
                .and_then(|yaml| choices_from_yaml(yaml).ok())
                .unwrap_or_default();
            RenderQuestionnaire::Absent { choices }
        }
        Some(definition) => {
            if for_reask || notation.state != StateName::BEGIN {
                answers.retain(|answer| answer.source != store::answers::SOURCE_EXTRACTED);
            }
            let values = values_for_answers(&answers, &codes);
            let route: BTreeSet<String> = definition
                .spec
                .question_order(&values)
                .into_iter()
                .map(|state| state.0)
                .collect();
            answers.retain(|answer| {
                answer
                    .state_name
                    .as_ref()
                    .or_else(|| codes.get(&answer.question_id))
                    .is_some_and(|state| route.contains(state))
            });
            RenderQuestionnaire::Present { definition, route }
        }
    };
    Ok(RenderAnswerProjection {
        notation,
        answers,
        codes,
        questionnaire,
    })
}

async fn verified_questionnaire(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<(store::notations::Notation, Option<QuestionnaireDefinition>), NotationSessionError> {
    let notation = store::notations::find_by_id(application_store, notation_id)
        .await?
        .ok_or(NotationSessionError::NotationNotFound(notation_id))?;
    let definition = if let Some(snapshot) = &notation.questionnaire_snapshot {
        Some(
            serde_json::from_value::<QuestionnaireDefinition>(snapshot.clone())
                .map_err(|error| NotationSessionError::SnapshotDecode(error.to_string()))?,
        )
    } else {
        let storage = storage.ok_or(NotationSessionError::QuestionnaireStorageRequired)?;
        let template = store::templates::find_by_id(application_store, notation.template_id)
            .await?
            .ok_or(NotationSessionError::NotationNotFound(notation_id))?;
        let body = store::templates::body_pinned_to(
            application_store,
            storage,
            &template,
            notation.template_body_sha256.as_deref(),
        )
        .await?;
        let definition = definition_from_verified_body(&body)?;
        // The seed strips frontmatter before storing bodies. Matching one of
        // those bodies proves metadata was discarded, not that intake was absent.
        // The catalog only classifies the missing authority; it never restores it.
        if definition.is_none()
            && store::seed::seeded_clause_sources(&template.code, &body).is_some()
            && catalog_spec_yaml(&template.code)
                .is_some_and(|yaml| definition_from_yaml(yaml).is_ok())
        {
            return Err(NotationSessionError::QuestionnaireAuthorityMissing);
        }
        definition
    };
    if let Some(definition) = &definition {
        definition.spec.validate()?;
        definition.spec.validate_routes(&definition.choices)?;
    }
    Ok((notation, definition))
}

fn definition_from_verified_body(
    body: &str,
) -> Result<Option<QuestionnaireDefinition>, NotationSessionError> {
    let Some(yaml) = rules::frontmatter::extract(body) else {
        return if body.trim_start().starts_with("---") {
            Err(WorkflowSpecError::Yaml("malformed template frontmatter".into()).into())
        } else {
            Ok(None)
        };
    };
    let value: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|error| WorkflowSpecError::Yaml(error.to_string()))?;
    if !value.is_mapping() {
        return Err(
            WorkflowSpecError::Yaml("template frontmatter must be a mapping".into()).into(),
        );
    }
    if value.get("questionnaire").is_none() {
        return Ok(None);
    }
    definition_from_yaml(yaml).map(Some)
}

/// The completed question chain for Lawyer Review and Re-ask. Extracted
/// proposals remain available to the active walker, but cannot replace a
/// confirmed branch selection when deciding which answers may be corrected.
///
/// # Errors
/// Returns notation, verified questionnaire, or answer-read errors.
pub async fn questionnaire_chain_for_reask(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<Vec<String>, NotationSessionError> {
    let projection = answer_projection(application_store, storage, notation_id, true).await?;
    let values = values_for_answers(&projection.answers, &projection.codes);
    Ok(match &projection.questionnaire {
        RenderQuestionnaire::Absent { .. } => Vec::new(),
        RenderQuestionnaire::Present { definition, .. } => definition
            .spec
            .question_order(&values)
            .into_iter()
            .map(|state| state.0)
            .collect(),
    })
}

/// The answers recorded for one notation, folded latest-wins into
/// `state name → recorded value` — the route resolver's input. Rows
/// arrive oldest-first, so the last row per state is the latest.
async fn recorded_answers(
    application_store: &store::application_store::ApplicationStore,
    notation_id: Uuid,
) -> Result<BTreeMap<String, String>, NotationSessionError> {
    let answers = store::answers::for_notation(application_store, notation_id).await?;
    Ok(values_for_answers(&answers, &BTreeMap::new()))
}

fn values_for_answers<'a>(
    answers: impl IntoIterator<Item = &'a store::answers::Answer>,
    codes: &BTreeMap<Uuid, String>,
) -> BTreeMap<String, String> {
    answers
        .into_iter()
        .filter_map(|answer| {
            answer
                .state_name
                .as_ref()
                .or_else(|| codes.get(&answer.question_id))
                .map(|state| (state.clone(), store::answers::display_value(&answer.value)))
        })
        .collect()
}

fn questionnaire_snapshot_from_definition(
    definition: &QuestionnaireDefinition,
) -> Result<serde_json::Value, NotationSessionError> {
    serde_json::to_value(definition)
        .map_err(|e| NotationSessionError::SnapshotEncode(e.to_string()))
}

/// Look up the question the questionnaire is *currently* asking,
/// without writing anything. Returns `QuestionnaireComplete` when
/// the questionnaire has already reached END.
pub async fn current_step(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<NextStep, NotationSessionError> {
    let (_, definition) = load_notation_and_spec(application_store, storage, notation_id).await?;
    let current_state = runtime
        .current_state(MachineKind::Questionnaire, notation_id)
        .await
        .unwrap_or_else(StateName::begin);
    next_step_from(application_store, &definition, notation_id, &current_state).await
}

/// Persist one answer, advance the questionnaire, and return the
/// next question — or `QuestionnaireComplete` if that answer
/// landed the machine at END.
///
/// `question_code` MUST match the question the runtime is
/// currently expecting; mismatches return [`NotationSessionError::QuestionMismatch`]
/// so a confused caller fails fast rather than silently writing an
/// answer against the wrong question.
///
/// `author` records who typed the answer and the authorship source (see
/// [`AnswerAuthor`]). The notation's bound Person stays the *respondent*
/// (`answers.person_id`) regardless of who entered it, so a lawyer-entered
/// and a client-entered answer to the same question share a respondent
/// but differ in authorship.
#[allow(clippy::too_many_arguments)]
pub async fn answer_step(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    question_code: &str,
    value: &str,
    author: AnswerAuthor<'_>,
) -> Result<NextStep, NotationSessionError> {
    answer_step_with_reference(
        application_store,
        runtime,
        storage,
        notation_id,
        question_code,
        value,
        None,
        author,
    )
    .await
}

/// [`answer_step`], plus the id of the existing row a DB-backed picker
/// selected for a record/reference question. `value` stays the row's
/// display name (what the placeholder renders); `reference_id` is embedded
/// in the stored envelope so the read-back surfaces `<state>.id`. Pass
/// `None` for a free-typed answer.
#[allow(clippy::too_many_arguments)]
pub async fn answer_step_with_reference(
    application_store: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    question_code: &str,
    value: &str,
    reference_id: Option<Uuid>,
    author: AnswerAuthor<'_>,
) -> Result<NextStep, NotationSessionError> {
    let (notation_row, definition) =
        load_notation_and_spec(application_store, storage, notation_id).await?;
    let person_id = notation_row.person_id;

    let current_state = runtime
        .current_state(MachineKind::Questionnaire, notation_id)
        .await
        .unwrap_or_else(StateName::begin);

    // Where the walk stands: the pointer rests on the last-answered
    // question (or BEGIN), and the route out of it — the recorded
    // answer's own choice edge on a branch state, `_` on a linear
    // one — names the question being answered now.
    let taken = recorded_answers(application_store, notation_id).await?;
    let prior = taken.get(current_state.as_str()).cloned();
    let condition = definition
        .spec
        .route_condition(&current_state, prior.as_deref())
        .ok_or(NotationSessionError::AlreadyComplete)?;
    let expected = definition
        .spec
        .route_from(&current_state, prior.as_deref())
        .ok_or(NotationSessionError::AlreadyComplete)?;
    if expected == StateName::end() {
        return Err(NotationSessionError::AlreadyComplete);
    }
    if expected.as_str() != question_code {
        return Err(NotationSessionError::QuestionMismatch {
            expected: expected.0,
            got: question_code.into(),
        });
    }

    let canonical_code = question_code_for_state(question_code);
    let question_row = store::questions::find_by_code(application_store, canonical_code)
        .await?
        .ok_or_else(|| NotationSessionError::QuestionNotSeeded(question_code.into()))?;
    // Close the declared choice set before writing, alongside the
    // question-code and completeness checks above: an off-list value must
    // not advance the walk, and must not be stored for a later render.
    ensure_declared_choice(&definition.choices, question_code, value)?;

    // The Answer row is application data; the worker doesn't know
    // about it, so we own the write here. Single insert — no txn.
    // `person_id` is the respondent; `authored_by`/`source` record who
    // actually entered it (lawyer on the client's behalf, or the client).
    store::answers::record(
        application_store,
        // The walked state name carries the `<type>__<role>` discriminator
        // (`entity__company`); the question row points at the bare code.
        &store::answers::NewAnswer::new(
            question_row.id,
            person_id,
            answer_value_for_state(question_code, value, reference_id),
        )
        .in_notation(notation_id, question_code)
        .authored_by(author.source, author.authored_by),
    )
    .await?;

    // `start` is idempotent; subsequent calls are no-ops. `signal`
    // advances state and (in production) triggers the worker's
    // `ctx.run` journal write — including stamping the answer
    // value as `payload`.
    runtime
        .start(
            MachineKind::Questionnaire,
            notation_id,
            definition.spec.inner(),
        )
        .await?;
    let signal_context = SignalContext {
        acting_person_id: author.authored_by.unwrap_or(person_id),
    };
    runtime
        .signal_with_context(
            MachineKind::Questionnaire,
            notation_id,
            &condition,
            Some(value),
            signal_context,
        )
        .await?;

    // If the next transition would land at END, fire the final
    // signal so the machine actually reaches END before we report
    // completion. The route out of the just-answered question
    // follows the answer just recorded.
    let next_after = definition.spec.route_from(&expected, Some(value));
    let final_condition = definition
        .spec
        .route_condition(&expected, Some(value))
        .ok_or_else(|| NotationSessionError::RouteDeadEnd {
            state: expected.0.clone(),
        })?;
    if matches!(&next_after, Some(s) if s == &StateName::end()) {
        runtime
            .signal_with_context(
                MachineKind::Questionnaire,
                notation_id,
                &final_condition,
                None,
                signal_context,
            )
            .await?;
        return Ok(NextStep::QuestionnaireComplete);
    }

    let next_state = next_after.ok_or_else(|| NotationSessionError::RouteDeadEnd {
        state: expected.0.clone(),
    })?;
    Ok(NextStep::NeedsAnswer {
        question: load_question(
            application_store,
            &next_state,
            &definition.prompts,
            &definition.choices,
        )
        .await?,
    })
}

/// The client's place in *their* portion of a notation's intake.
///
/// The client sees only the questions whose `audience` is `client` or
/// `both` ([`store::questions::is_client_facing`]), in spec order.
/// Unlike [`answer_step`], the client surface does **not** drive the
/// questionnaire runtime — that pointer is lawyer's progress toward the
/// post-intake workflow. The client's answers are written straight to the
/// `answers` table ([`record_client_answer`]); reads key the latest answer
/// by the authored questionnaire state (`state_name`), with a bare-code
/// fallback only for rows that predate state-scoped answer writes, so a
/// client edit lands without disturbing lawyer's walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientIntakeStep {
    /// The client should answer (or confirm) this question. `prior_value`
    /// pre-fills any current answer — including one lawyer entered on the
    /// client's behalf — so the client confirms rather than re-types.
    NeedsAnswer {
        question: QuestionDescriptor,
        prior_value: Option<String>,
        /// 1-based position among this notation's client-facing questions.
        position: usize,
        /// Count of client-facing questions on this notation.
        total: usize,
        /// The client-facing question codes, BEGIN → END order — the chain a
        /// step-list progress rail walks. The same ordering `position`/`total`
        /// are computed from, so a caller wanting both never re-derives it.
        steps: Vec<String>,
    },
    /// The client has answered every client-facing question; the rest is
    /// the firm's to finish.
    Complete { total: usize },
}

/// The `custom_*` primitives whose seeded `help_text` speaks to the
/// template author choosing a question type ("Use this for a one-off text
/// answer when no glossary type fits."), not to the person answering it.
/// The other custom primitives (`custom_phone`, `custom_usd`,
/// `custom_datetime`) carry respondent guidance and stay visible. This is
/// the audience decision `help_text` lacks: `questions.audience` gates which
/// questions a client sees, and this list gates which guidance they read on
/// them. A seed-level split of author notes from respondent help is the
/// durable form; until then a test pins this list to the seed so a new
/// author-facing string cannot reach a client unnoticed.
const AUTHOR_FACING_HELP_CODES: [&str; 4] = [
    "custom_text",
    "custom_yes_no",
    "custom_single_choice",
    "custom_multiple_choice",
];

/// Whether the seeded `help_text` behind questionnaire `state` is written
/// for the template author rather than the respondent, keyed by the
/// canonical code with its `__role` suffix stripped.
fn is_author_facing_help(state: &str) -> bool {
    AUTHOR_FACING_HELP_CODES.contains(&question_code_for_state(state))
}

/// Resolve where the client is in their portion of `notation_id`'s
/// intake: the first client-facing question the client has not yet
/// answered (no `client`-sourced answer), pre-filled with any current
/// value, or [`ClientIntakeStep::Complete`] when the client has answered
/// them all. Save-per-step: a drop-off resumes at the first question
/// still missing a client answer. The descriptor is the lawyer walk's,
/// less any author-facing `help_text` ([`AUTHOR_FACING_HELP_CODES`]).
pub async fn client_intake_step(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<ClientIntakeStep, NotationSessionError> {
    client_intake_step_at(application_store, storage, notation_id, None).await
}

/// Resolve the client's first unanswered question, or a previously visited
/// client-facing question at or before that frontier. This permits correction
/// while preventing a crafted request from skipping unanswered intake steps.
pub async fn client_intake_step_at(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    requested_code: Option<&str>,
) -> Result<ClientIntakeStep, NotationSessionError> {
    let (notation_row, definition) =
        load_notation_and_spec(application_store, storage, notation_id).await?;
    let person_id = notation_row.person_id;

    // Route-aware: the walk's answers so far select the branches, so a
    // question on a pruned branch is no longer part of the client's
    // portion at all (and a still-unanswered branch fans out until its
    // own choice is made).
    let taken = recorded_answers(application_store, notation_id).await?;
    let codes: Vec<String> = definition
        .spec
        .question_order(&taken)
        .into_iter()
        .map(|state| state.0)
        .collect();
    let canonical_codes: Vec<String> = codes
        .iter()
        .map(|code| question_code_for_state(code).to_string())
        .collect();
    let rows = store::questions::find_by_codes(application_store, &canonical_codes).await?;
    let by_code: BTreeMap<String, store::questions::Question> =
        rows.into_iter().map(|q| (q.code.clone(), q)).collect();
    let id_to_code: BTreeMap<Uuid, String> =
        by_code.values().map(|q| (q.id, q.code.clone())).collect();

    // Client-facing questions, in spec order.
    let client_codes: Vec<String> = codes
        .iter()
        .filter(|c| {
            metadata_lookup(&definition.audiences, c).map_or_else(
                || {
                    by_code
                        .get(question_code_for_state(c))
                        .is_some_and(|q| store::questions::is_client_facing(&q.audience))
                },
                |audience| store::questions::is_client_facing(audience),
            )
        })
        .cloned()
        .collect();
    let total = client_codes.len();

    // One pass over the respondent's answers: latest value per code (for
    // pre-fill) and the set of codes the client has answered themselves.
    let answers =
        store::answers::for_person_in_notation(application_store, person_id, notation_id).await?;
    let mut latest_value: BTreeMap<String, String> = BTreeMap::new();
    let mut client_answer_counts: BTreeMap<String, usize> = BTreeMap::new();
    for a in answers {
        let Some(canonical_code) = id_to_code.get(&a.question_id) else {
            continue;
        };
        let answer_code = a
            .state_name
            .clone()
            .unwrap_or_else(|| canonical_code.clone());
        if a.source == store::answers::SOURCE_CLIENT {
            *client_answer_counts.entry(answer_code.clone()).or_default() += 1;
        }
        latest_value.insert(answer_code, store::answers::display_value(&a.value));
    }
    let client_answered = answered_client_states(&client_codes, client_answer_counts);

    let frontier = client_codes
        .iter()
        .position(|code| !client_answered.contains(code))
        .unwrap_or(client_codes.len());

    // Once every client-facing step has an answer, stale question codes must
    // not reopen the completed client intake. Corrections remain available
    // while the journey is active and append new Answer rows at that point.
    if frontier == client_codes.len() {
        return Ok(ClientIntakeStep::Complete { total });
    }

    let requested_index = client_question_index(&client_codes, frontier, requested_code)?;
    for (idx, code) in client_codes.iter().enumerate() {
        let requested = requested_index == Some(idx);
        if !requested && client_answered.contains(code) {
            continue;
        }
        if requested || idx == frontier {
            let mut question = load_question(
                application_store,
                &StateName::from(code.as_str()),
                &definition.prompts,
                &definition.choices,
            )
            .await?;
            if is_author_facing_help(code) {
                question.help_text = None;
            }
            let mut prior_value = latest_value
                .get(code)
                .or_else(|| latest_value.get(question_code_for_state(code)))
                .cloned();
            if prior_value.is_none() {
                prior_value = address_prior_value(application_store, person_id, code).await?;
            }
            return Ok(ClientIntakeStep::NeedsAnswer {
                question,
                prior_value,
                position: idx + 1,
                total,
                steps: client_codes.clone(),
            });
        }
    }
    Ok(ClientIntakeStep::Complete { total })
}

fn client_question_index(
    client_codes: &[String],
    frontier: usize,
    requested_code: Option<&str>,
) -> Result<Option<usize>, NotationSessionError> {
    let Some(requested) = requested_code else {
        return Ok(None);
    };
    let Some(idx) = client_codes.iter().position(|code| code == requested) else {
        return Err(NotationSessionError::QuestionNotClientFacing(
            requested.to_string(),
        ));
    };
    if idx > frontier {
        return Err(NotationSessionError::QuestionMismatch {
            expected: client_codes.get(frontier).cloned().unwrap_or_default(),
            got: requested.to_string(),
        });
    }
    Ok(Some(idx))
}

/// LAW-57: when a client-facing, address-shaped question (`address__…` or
/// bare `address`) has no answer on file yet, pre-fill it from the
/// respondent's own stored [`store::addresses::Address`] rather than
/// leaving the field blank. Scoped deliberately narrow: this only ever
/// reads the *notation's own client* (`person_id`) — never another role's
/// address — and only when the question's type token is `address` or
/// `addresses` (the same closed vocabulary `rules::f115`'s field-shape
/// registry uses). Returns the most recently recorded address as one
/// display line, the same shape a lawyer's free-typed address answer takes
/// today (see `TEST_ENTITY_ADDRESS` in this module's tests) — there is no
/// structured "address answer" shape to match.
async fn address_prior_value(
    application_store: &store::application_store::ApplicationStore,
    person_id: Uuid,
    code: &str,
) -> Result<Option<String>, NotationSessionError> {
    let type_token = code
        .split_once("__")
        .map_or(code, |(type_token, _)| type_token);
    if !matches!(type_token, "address" | "addresses") {
        return Ok(None);
    }
    Ok(store::addresses::for_person(application_store, person_id)
        .await?
        .into_iter()
        .next_back()
        .map(|address| format_address_line(&address)))
}

/// One display line for a stored [`store::addresses::Address`] —
/// `line1[, line2], city, region postal_code, country`.
fn format_address_line(address: &store::addresses::Address) -> String {
    let mut parts = vec![address.line1.clone()];
    if let Some(line2) = address.line2.as_ref().filter(|line2| !line2.is_empty()) {
        parts.push(line2.clone());
    }
    parts.push(format!(
        "{}, {} {}",
        address.city, address.region, address.postal_code
    ));
    parts.push(address.country.clone());
    parts.join(", ")
}

/// Record one client-sourced answer to a client-facing question on
/// `notation_id`, without advancing the lawyer questionnaire runtime.
/// Rejects a question that is lawyer-only or outside the notation's
/// questionnaire so a hand-crafted POST can't write an arbitrary answer.
pub async fn record_client_answer(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    question_code: &str,
    value: &str,
    authored_by: Uuid,
) -> Result<(), NotationSessionError> {
    record_client_answer_with_reference(
        application_store,
        storage,
        notation_id,
        question_code,
        value,
        None,
        authored_by,
    )
    .await
}

/// [`record_client_answer`], plus the id of the existing row a DB-backed
/// picker selected. Mirrors [`answer_step_with_reference`] on the client
/// self-serve surface: `value` is the row's display name, `reference_id`
/// is embedded in the stored envelope. Pass `None` for a free-typed answer.
#[allow(clippy::too_many_arguments)]
pub async fn record_client_answer_with_reference(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    question_code: &str,
    value: &str,
    reference_id: Option<Uuid>,
    authored_by: Uuid,
) -> Result<(), NotationSessionError> {
    let (notation_row, definition) =
        load_notation_and_spec(application_store, storage, notation_id).await?;
    let taken = recorded_answers(application_store, notation_id).await?;
    if !definition
        .spec
        .question_order(&taken)
        .iter()
        .any(|state| state.as_str() == question_code)
    {
        return Err(NotationSessionError::QuestionNotClientFacing(
            question_code.into(),
        ));
    }
    let canonical_code = question_code_for_state(question_code);
    let question_row = store::questions::find_by_code(application_store, canonical_code)
        .await?
        .ok_or_else(|| NotationSessionError::QuestionNotSeeded(question_code.into()))?;
    let audience = metadata_lookup(&definition.audiences, question_code)
        .map_or(question_row.audience.as_str(), String::as_str);
    if !store::questions::is_client_facing(audience) {
        return Err(NotationSessionError::QuestionNotClientFacing(
            question_code.into(),
        ));
    }
    ensure_declared_choice(&definition.choices, question_code, value)?;
    store::answers::record(
        application_store,
        &store::answers::NewAnswer::new(
            question_row.id,
            notation_row.person_id,
            answer_value_for_state(question_code, value, reference_id),
        )
        .in_notation(notation_id, question_code)
        .authored_by(store::answers::SOURCE_CLIENT, Some(authored_by)),
    )
    .await?;
    Ok(())
}

/// Check a whole correction set before writing any re-collected answer.
/// The caller still checks the workflow state and flagged set. A correction
/// may change a choice while preserving its question chain; changing which
/// questions were asked requires a fresh intake, not reuse of off-route answers.
///
/// # Errors
/// Returns notation/snapshot/read errors, an undeclared choice, an off-route
/// question, or a correction that changes the completed question chain.
pub async fn validate_reask_answers(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    answers: &BTreeMap<String, String>,
) -> Result<(), NotationSessionError> {
    let projection = answer_projection(application_store, storage, notation_id, true).await?;
    check_reask_projection(&projection, answers)
}

fn check_reask_projection(
    projection: &RenderAnswerProjection,
    answers: &BTreeMap<String, String>,
) -> Result<(), NotationSessionError> {
    match &projection.questionnaire {
        RenderQuestionnaire::Absent { .. } if answers.is_empty() => Ok(()),
        RenderQuestionnaire::Absent { .. } => Err(NotationSessionError::QuestionNotOnRoute),
        RenderQuestionnaire::Present { definition, .. } => check_reask_route(
            definition,
            &values_for_answers(&projection.answers, &projection.codes),
            answers,
        ),
    }
}

fn check_reask_route(
    definition: &QuestionnaireDefinition,
    taken: &BTreeMap<String, String>,
    answers: &BTreeMap<String, String>,
) -> Result<(), NotationSessionError> {
    let route = definition.spec.question_order(taken);
    for (code, value) in answers {
        if !route.iter().any(|state| state.as_str() == code.as_str()) {
            return Err(NotationSessionError::QuestionNotOnRoute);
        }
        ensure_declared_choice(&definition.choices, code, value)?;
    }
    let mut corrected = taken.clone();
    corrected.extend(answers.clone());
    if definition.spec.question_order(&corrected) != route {
        return Err(NotationSessionError::QuestionnaireRouteChanged);
    }
    Ok(())
}

/// Re-collect one flagged answer on a notation parked at `reask__client`
/// after a `lawyer_review` change request. Writes a fresh, latest-wins answer
/// row attributed to `author` — the client self-serve, or lawyer on their
/// behalf — **without** touching the completed questionnaire runtime.
///
/// Answers and questions are decoupled: a correction re-collects a specific
/// answer, it never re-walks intake, so the questionnaire pointer (already at
/// END) stays put and every other answer is left as-is. The write is gated to
/// the flagged set ([`store::reask::flagged_questions`]) — a question the
/// review did not flag is rejected, so a hand-crafted POST can't rewrite an
/// un-flagged answer. The correction also preserves the completed question
/// chain; a different route requires a fresh intake. Audience gating (client-facing only) is the caller's,
/// applied on the client self-serve surface; lawyer-on-behalf may re-collect
/// any flagged question.
///
/// Subsequent document assembly uses the corrected answers in the review context.
pub async fn record_reask_answer(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    question_code: &str,
    value: &str,
    reference_id: Option<Uuid>,
    author: AnswerAuthor<'_>,
) -> Result<(), NotationSessionError> {
    // The verified frozen or pinned questionnaire supplies the declared
    // choices and completed route before any correction is appended.
    let projection = answer_projection(application_store, storage, notation_id, true).await?;
    let notation_row = &projection.notation;
    let flagged = store::reask::flagged_questions(application_store, notation_id).await?;
    if !flagged.iter().any(|c| c == question_code) {
        return Err(NotationSessionError::QuestionNotFlagged(
            question_code.into(),
        ));
    }
    check_reask_projection(
        &projection,
        &BTreeMap::from([(question_code.to_owned(), value.to_owned())]),
    )?;
    let canonical_code = question_code_for_state(question_code);
    let question_row = store::questions::find_by_code(application_store, canonical_code)
        .await?
        .ok_or_else(|| NotationSessionError::QuestionNotSeeded(question_code.into()))?;
    store::answers::record(
        application_store,
        &store::answers::NewAnswer::new(
            question_row.id,
            notation_row.person_id,
            answer_value_for_state(question_code, value, reference_id),
        )
        .in_notation(notation_id, question_code)
        .authored_by(author.source, author.authored_by),
    )
    .await?;
    Ok(())
}

async fn load_notation_and_spec(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
) -> Result<(store::notations::Notation, QuestionnaireDefinition), NotationSessionError> {
    let notation_row = store::notations::find_by_id(application_store, notation_id)
        .await?
        .ok_or(NotationSessionError::NotationNotFound(notation_id))?;

    // Resolve against the frozen snapshot, immune to later template/binary
    // changes. Only a Notation created before the snapshot column
    // (`questionnaire_snapshot IS NULL`) re-resolves from the template.
    if let Some(snapshot) = &notation_row.questionnaire_snapshot {
        let definition = serde_json::from_value(snapshot.clone())
            .map_err(|e| NotationSessionError::SnapshotDecode(e.to_string()))?;
        return Ok((notation_row, definition));
    }
    let template_row = store::templates::find_by_id(application_store, notation_row.template_id)
        .await?
        .ok_or(NotationSessionError::NotationNotFound(notation_id))?;
    let definition =
        questionnaire_definition_for(application_store, storage, &template_row).await?;
    Ok((notation_row, definition))
}

/// Resolve a template's questionnaire spec.
///
/// A **project-scoped** row whose blob body carries its own `questionnaire:`
/// block wins: a Project that took the trouble to version its own
/// questionnaire must not have it silently shadowed by the compile-time
/// binary, so its blob is parsed even when the `code` also has a bundled spec.
///
/// A scoped row that overrides only the *document body* (a blob with no
/// `questionnaire:` block) still uses the bundled questionnaire — bundled
/// codes may be body-only overridden per Project. Bundled rows, blob-less
/// scoped rows, and scoped rows whose blob can't be read likewise fall back to
/// the bundled standalone YAML (compile-time `include_str!`). A non-bundled
/// template parses its spec from the markdown body; one with no body in
/// storage (or no `storage` handle) cannot drive a questionnaire and surfaces
/// [`NotationSessionError::TemplateHasNoQuestionnaire`].
async fn questionnaire_definition_for(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    template_row: &store::templates::Template,
) -> Result<QuestionnaireDefinition, NotationSessionError> {
    // A project-scoped row that carries its own questionnaire blob wins over
    // the bundled YAML — but only if the blob body actually declares a
    // `questionnaire:` block. A body-only override (or an unreadable asset)
    // keeps the bundled questionnaire, so a scoped body customization never
    // breaks intake for a bundled code.
    if template_row.project_id.is_some() && template_row.asset_id.is_some() {
        if let Some(storage) = storage {
            if let Ok(body) = store::templates::body(application_store, storage, template_row).await
            {
                if template_has_questionnaire(&body) {
                    return definition_from_body(&body);
                }
            }
        }
    }
    if let Some(yaml) = catalog_spec_yaml(&template_row.code) {
        return definition_from_yaml(yaml);
    }
    let storage = storage.ok_or_else(|| {
        NotationSessionError::TemplateHasNoQuestionnaire(template_row.code.clone())
    })?;
    let body = store::templates::body(application_store, storage, template_row)
        .await
        .map_err(|_| NotationSessionError::TemplateHasNoQuestionnaire(template_row.code.clone()))?;
    definition_from_body(&body)
}

/// Build a [`QuestionnaireDefinition`] from a bundled standalone spec YAML.
/// Runs the full questionnaire gate — shape validation plus route
/// coverage against the parsed `choices:` — so a template with an
/// unreachable question, a cycle, or an uncovered answer fails here,
/// at compile time, never mid-walk.
fn definition_from_yaml(yaml: &str) -> Result<QuestionnaireDefinition, NotationSessionError> {
    let prompts = prompt_overrides_from_yaml(yaml)?;
    let choices = choices_from_yaml(yaml)?;
    let spec = questionnaire_spec_from_yaml(yaml)?;
    spec.validate_routes(&choices)?;
    Ok(QuestionnaireDefinition {
        spec,
        prompts,
        audiences: audiences_from_yaml(yaml)?,
        choices,
    })
}

/// Build a [`QuestionnaireDefinition`] from a template's markdown body
/// (its `questionnaire:` frontmatter block). Runs the same route gate as
/// [`definition_from_yaml`] — see there.
fn definition_from_body(body: &str) -> Result<QuestionnaireDefinition, NotationSessionError> {
    let prompts = prompt_overrides_from_template(body)?;
    let choices = choices_from_template(body)?;
    let spec = questionnaire_spec_from_template(body)?;
    spec.validate_routes(&choices)?;
    Ok(QuestionnaireDefinition {
        spec,
        prompts,
        audiences: audiences_from_template(body)?,
        choices,
    })
}

async fn first_step(
    application_store: &store::application_store::ApplicationStore,
    definition: &QuestionnaireDefinition,
    notation_id: Uuid,
) -> Result<NextStep, NotationSessionError> {
    next_step_from(
        application_store,
        definition,
        notation_id,
        &StateName::begin(),
    )
    .await
}

/// The question now being asked, given the machine's pointer: the
/// pointer rests on the last-answered question (or BEGIN), and the
/// route out of it — the recorded answer's own choice edge on a
/// branch state, `_` on a linear one — names the next question.
async fn next_step_from(
    application_store: &store::application_store::ApplicationStore,
    definition: &QuestionnaireDefinition,
    notation_id: Uuid,
    current_state: &StateName,
) -> Result<NextStep, NotationSessionError> {
    let taken = recorded_answers(application_store, notation_id).await?;
    let prior = taken.get(current_state.as_str()).cloned();
    let Some(next) = definition.spec.route_from(current_state, prior.as_deref()) else {
        return Ok(NextStep::QuestionnaireComplete);
    };
    if next == StateName::end() {
        return Ok(NextStep::QuestionnaireComplete);
    }
    Ok(NextStep::NeedsAnswer {
        question: load_question(
            application_store,
            &next,
            &definition.prompts,
            &definition.choices,
        )
        .await?,
    })
}

async fn load_question(
    application_store: &store::application_store::ApplicationStore,
    state: &StateName,
    prompts: &BTreeMap<String, String>,
    choices: &BTreeMap<String, BTreeMap<String, String>>,
) -> Result<QuestionDescriptor, NotationSessionError> {
    let code = question_code_for_state(state.as_str());
    let row = store::questions::find_by_code(application_store, code)
        .await?
        .ok_or_else(|| NotationSessionError::QuestionNotSeeded(state.0.clone()))?;
    // A template prompt may name the question by interpolation (`What is the
    // {{for_label}}?`) instead of restating the role. The bank prompt already
    // does; an override gets the same substitution.
    let raw = prompt_override_for_state(prompts, state.as_str()).unwrap_or(row.prompt.as_str());
    let prompt = localize_prompt_for_state(raw, state.as_str());
    Ok(QuestionDescriptor {
        id: row.id,
        code: state.0.clone(),
        prompt,
        answer_type: row.answer_type,
        choices: choices_for_state(choices, state.as_str()),
        help_text: store::seed::question_help_text(code),
    })
}

/// Persist one machine-extracted answer for a notation's respondent, keyed by
/// the questionnaire `state_name` and tagged [`answer::SOURCE_EXTRACTED`].
///
/// This is the batch-coverage sibling of [`answer_step`]: it converges on the
/// same one-row-per-question `answers` write, but does **not** touch the state
/// machine. Transcript coverage proposes answers out of walk order, so it
/// cannot drive the questionnaire forward — it seeds proposed defaults that the
/// walk then surfaces (via the latest-per-state read-back) for a lawyer to confirm
/// or edit. Returns `false` when `state_name`'s registry question isn't seeded
/// (skipped, mirroring [`seed`]-style writers); `true` once a row is inserted.
///
/// Takes `storage` because resolving the questionnaire is what supplies the
/// declared choice set a proposal is checked against, and a notation created
/// directly — rather than through [`start_notation`] — carries no frozen
/// snapshot, so its spec is read from the template body in storage.
pub async fn record_extracted_answer(
    application_store: &store::application_store::ApplicationStore,
    storage: Option<&Arc<dyn StorageService>>,
    notation_id: Uuid,
    state_name: &str,
    value: &str,
) -> Result<bool, String> {
    // The notation and its questionnaire in one load: the spec supplies the
    // declared choice set this proposal is closed against.
    let (notation_row, definition) =
        match load_notation_and_spec(application_store, storage, notation_id).await {
            Ok(loaded) => loaded,
            // A vanished notation is skipped, mirroring the unseeded-question
            // arm below rather than failing the whole coverage run.
            Err(NotationSessionError::NotationNotFound(_)) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
    let canonical = question_code_for_state(state_name);
    let Some(question_row) = store::questions::find_by_code(application_store, canonical)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    // A transcript proposal is read back by the render exactly like a typed
    // answer (latest-per-state wins, with no filter on `source`), so an
    // off-list extraction is refused here too. It is *skipped* rather than
    // an error: the caller reports a skipped finding as uncovered, which is
    // the right outcome — the lawyer is asked the question instead of being
    // shown a proposal the questionnaire never offered.
    if ensure_declared_choice(&definition.choices, state_name, value).is_err() {
        return Ok(false);
    }
    let author = AnswerAuthor::extracted();
    store::answers::record(
        application_store,
        &store::answers::NewAnswer::new(
            question_row.id,
            notation_row.person_id,
            // The same envelope a typed answer carries, so the read-back is
            // exactly like a typed answer — including the dotted fields. A
            // record state (`project__engagement`) must expose `.name` or a
            // document rendering `{{project__engagement.name}}` comes out
            // empty even though the proposal is on record.
            answer_value_for_state(state_name, value, None),
        )
        .in_notation(notation_id, state_name)
        .authored_by(author.source, author.authored_by),
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Build the stored `answers.value` envelope for one answer. Singular
/// record/reference states mirror the row they create or select
/// (`{"value":name,"name":name}`); when the answer picked an existing row
/// by id (a DB-backed picker selection), that row's `id` is embedded so
/// the read-back surfaces `<state>.id` without changing what a placeholder
/// renders (`value`/`name` stay the display string). Everything else is a
/// primitive `{"value":…}`.
fn answer_value_for_state(
    state: &str,
    value: &str,
    reference_id: Option<Uuid>,
) -> serde_json::Value {
    match question_code_for_state(state) {
        "person" | "entity" | "project" | "jurisdiction" | "country" => {
            let mut envelope = serde_json::json!({ "value": value, "name": value });
            if let Some(id) = reference_id {
                envelope["id"] = serde_json::json!(id);
            }
            envelope
        }
        _ => store::answers::primitive(value),
    }
}

fn prompt_override_for_state<'a>(
    prompts: &'a BTreeMap<String, String>,
    state: &str,
) -> Option<&'a str> {
    metadata_lookup(prompts, state).map(String::as_str)
}

fn choices_for_state(
    choices: &BTreeMap<String, BTreeMap<String, String>>,
    state: &str,
) -> Vec<QuestionChoice> {
    metadata_lookup(choices, state)
        .into_iter()
        .flat_map(|entries| entries.iter())
        .map(|(value, label)| QuestionChoice {
            value: value.clone(),
            label: label.clone(),
        })
        .collect()
}

fn metadata_lookup<'a, T>(map: &'a BTreeMap<String, T>, state: &str) -> Option<&'a T> {
    metadata_keys_for_state(state)
        .into_iter()
        .find_map(|key| map.get(key))
}

/// Resolve a stored choice `value` to its human label for a question
/// `state`, given the template's choice metadata (`value → label`
/// keyed by custom-question key / question code — see
/// [`choices_from_yaml`]). Returns `None` for a free-text state
/// (no choice metadata) or a value that isn't a declared option, so the
/// caller falls back to the raw value. Lets a rendered document show the
/// label ("Married"), not the stored key ("married"), everywhere the
/// walker would have shown the label.
#[must_use]
pub fn choice_label(
    choices: &BTreeMap<String, BTreeMap<String, String>>,
    state: &str,
    value: &str,
) -> Option<String> {
    store::question_registry::choice_label(choices, state, value)
}

/// Whether `state`'s answer is a single declared choice key, so it can be
/// checked against the declared set.
///
/// `custom_single_choice` and `custom_yes_no` store exactly one key. A
/// `custom_multiple_choice` answer is a *set* of keys rather than one, and
/// needs the checkbox-group field shape that ENG-454 tracks separately — so
/// it is deliberately left open here rather than closed against a check that
/// would reject a legitimate multi-value answer. Record and reference types
/// declare no YAML choices at all; their closed set is the database
/// candidate list, matched where the pick is resolved.
fn stores_one_declared_choice(state: &str) -> bool {
    use store::question_registry::QuestionType;
    matches!(
        QuestionType::from_state_name(state),
        Some(
            QuestionType::CustomSingleChoice
                | QuestionType::CustomYesNo
                | QuestionType::CustomMultipleChoice
        )
    )
}

/// Whether `state` stores several selections as one JSON array answer
/// (`multiple_choice`), rather than one scalar choice key.
fn is_multi_valued_choice(state: &str) -> bool {
    use store::question_registry::QuestionType;
    matches!(
        QuestionType::from_state_name(state),
        Some(QuestionType::CustomMultipleChoice)
    )
}

/// Refuse an answer that names an option its question never declared — the
/// closed-choice check every answer write shares.
///
/// A choice question declares a closed `value → label` set
/// (`choices.<key>`). The render side resolves a stored
/// value through [`choice_label`] and falls back to the raw string when the
/// value is not a declared option, so an undeclared value is substituted
/// verbatim into whatever the template says — including the engagement
/// letter's governing-law and arbitration clause, whose choices are
/// `nevada`/`california`/`washington`. Closing the set at render time is not
/// enough: the answer is already stored, and a document assembled from
/// stored answers is what a client signs.
///
/// So the set is closed here, where an answer enters. The browser's radio
/// group is the only surface that *cannot* post an off-list value; the CLI,
/// the REST command boundary, the Navigator MCP tool surface, and a hand-crafted POST
/// all can.
///
/// Reads the declared set through the same [`metadata_lookup`] the render
/// side uses, so the guard and the label resolution cannot drift: exactly
/// the values [`choice_label`] can map are the values this accepts.
fn ensure_declared_choice(
    choices: &BTreeMap<String, BTreeMap<String, String>>,
    state: &str,
    value: &str,
) -> Result<(), NotationSessionError> {
    if !stores_one_declared_choice(state) {
        return Ok(());
    }
    // No declared set means "not a choice question" (a free-text or date
    // custom primitive), not "no valid options" — leave it alone.
    let Some(declared) = metadata_lookup(choices, state).filter(|d| !d.is_empty()) else {
        return Ok(());
    };
    let undeclared = || NotationSessionError::UndeclaredChoice {
        state: state.to_string(),
        declared: declared.keys().cloned().collect(),
    };
    if is_multi_valued_choice(state) {
        // `value` is the JSON array `record_answer_value`/`answer_step` were
        // handed — every selection must be declared, or the whole answer is
        // refused (an empty selection, `[]`, is a real "chose none of these"
        // and always accepted). Malformed JSON cannot have come from this
        // question's own control, so it is refused the same as an
        // off-list value.
        let selected: Vec<String> = serde_json::from_str(value).map_err(|_| undeclared())?;
        return selected
            .iter()
            .all(|v| declared.contains_key(v))
            .then_some(())
            .ok_or_else(undeclared);
    }
    if declared.contains_key(value) {
        return Ok(());
    }
    Err(undeclared())
}

fn answered_client_states(
    client_codes: &[String],
    mut answer_counts_by_code: BTreeMap<String, usize>,
) -> std::collections::BTreeSet<String> {
    let mut answered = std::collections::BTreeSet::new();
    for code in client_codes {
        if let Some(remaining) = answer_counts_by_code.get_mut(code) {
            if *remaining > 0 {
                answered.insert(code.clone());
                *remaining -= 1;
                continue;
            }
        }
        if let Some(remaining) = answer_counts_by_code.get_mut(question_code_for_state(code)) {
            if *remaining > 0 {
                answered.insert(code.clone());
                *remaining -= 1;
            }
        }
    }
    answered
}

/// Substitute a question prompt's `{{for_label}}` / `{{label}}` / `{label}`
/// placeholders with the state's role discriminator (`person__client` →
/// `client`), the same way the walker renders a step. Public so a review
/// surface listing the questionnaire's questions (the "Request changes" and
/// re-ask surfaces) renders the same clean label the client saw, rather than
/// the raw templated prompt.
#[must_use]
pub fn localize_prompt_for_state(prompt: &str, state: &str) -> String {
    let label = state
        .split_once("__")
        .map_or(state, |(_, label)| label)
        .replace('_', " ");
    prompt
        .replace("{{for_label}}", &label)
        .replace("{{label}}", &label)
        .replace("{label}", &label)
}

#[cfg(test)]
mod tests {
    use super::{
        answer_value_for_state, answered_client_states, is_author_facing_help,
        localize_prompt_for_state, NotationSessionError, AUTHOR_FACING_HELP_CODES,
    };

    #[test]
    fn a_store_or_template_write_failure_is_not_a_lawyer_fault() {
        let store = NotationSessionError::TemplateSource(
            store::template_source::TemplateSourceError::Store("database: down".into()),
        );
        assert!(store.lawyer_fault().is_none());
        let template =
            NotationSessionError::Template(store::templates::TemplateError::MissingSourceCommit);
        assert!(template.lawyer_fault().is_none());
    }

    #[test]
    fn a_what_is_prompt_interpolates_the_question_label() {
        assert_eq!(
            localize_prompt_for_state("What is the {{for_label}}?", "custom_usd__annual_salary"),
            "What is the annual salary?"
        );
    }

    #[test]
    fn author_facing_help_allowlist_matches_the_seed() {
        // The allowlist is a snapshot of today's `Question.yaml`: every
        // seeded string that opens by telling the *author* when to use the
        // question ("Use this …") must be on it, and nothing else may be.
        // A new author-facing string, or one of the four rewritten for the
        // respondent, fails here so the client boundary moves deliberately.
        for code in store::question_registry::QuestionType::all_tokens() {
            let help = store::seed::question_help_text(code)
                .unwrap_or_else(|| panic!("question `{code}` has no help_text"));
            let reads_as_author_note = help.starts_with("Use this");
            assert_eq!(
                reads_as_author_note,
                AUTHOR_FACING_HELP_CODES.contains(&code),
                "question `{code}` help_text {help:?}: author-facing wording and \
                 AUTHOR_FACING_HELP_CODES disagree",
            );
        }
        // The role suffix a questionnaire state carries is stripped first,
        // and a respondent-facing custom primitive is not caught by prefix.
        assert!(is_author_facing_help("custom_text__engagement_scope"));
        assert!(is_author_facing_help("custom_multiple_choice"));
        assert!(!is_author_facing_help("custom_phone__daytime"));
        assert!(!is_author_facing_help("person__client"));
    }

    #[test]
    fn reference_envelope_embeds_the_selected_row_id() {
        // A record/reference state mirrors the row: value/name are the
        // display string; a picker selection embeds the row id so the
        // read-back surfaces `<state>.id` without changing what renders.
        let picked = Uuid::now_v7();
        let with_id = answer_value_for_state("country__of_birth", "Mexico", Some(picked));
        assert_eq!(with_id["value"], "Mexico");
        assert_eq!(with_id["name"], "Mexico");
        assert_eq!(with_id["id"], picked.to_string());

        // No selection → the same envelope without an id (a free-typed row).
        let no_id = answer_value_for_state("entity__company", "Bright Star", None);
        assert_eq!(no_id["value"], "Bright Star");
        assert_eq!(no_id["name"], "Bright Star");
        assert!(no_id.get("id").is_none());

        // A custom primitive is unchanged — never grows an id field.
        let primitive = answer_value_for_state("custom_text__note", "hello", Some(picked));
        assert_eq!(primitive, store::answers::primitive("hello"));
    }

    use std::collections::BTreeMap;

    use uuid::Uuid;

    #[test]
    fn answered_client_states_do_not_collapse_duplicate_typed_prefixes() {
        let codes = vec![
            "custom_text__mission_statement".to_string(),
            "custom_text__revenue_strategy".to_string(),
        ];
        let answered = answered_client_states(&codes, BTreeMap::from([("custom_text".into(), 1)]));

        assert!(answered.contains("custom_text__mission_statement"));
        assert!(!answered.contains("custom_text__revenue_strategy"));
    }

    // --- ENG-459: the declared choice set is closed at write time ---

    /// Not one of the engagement letter's declared options, and a term no
    /// client would agree to — the shape of value the render side would
    /// otherwise substitute verbatim into the governing-law and arbitration
    /// clause.
    const OFF_LIST_GOVERNING_LAW: &str =
        "the Cayman Islands, and the Firm disclaims all liability for its own work";

    /// The declared options, as `UndeclaredChoice` reports them (the
    /// declared map's key order).
    const DECLARED_GOVERNING_LAW: [&str; 3] = ["california", "nevada", "washington"];

    #[test]
    fn multiple_choice_stores_one_declared_choice_too() {
        use super::stores_one_declared_choice;
        // ENG-505: a `custom_multiple_choice` answer is a set of keys, not one
        // key, so it cannot close against a single-key lookup the way
        // single-choice/yes-no do — `ensure_declared_choice` branches on
        // `is_multi_valued_choice` to check each selected key instead.
        assert!(stores_one_declared_choice(
            "custom_single_choice__governing_law"
        ));
        assert!(stores_one_declared_choice("custom_yes_no__has_counsel"));
        assert!(stores_one_declared_choice(
            "custom_multiple_choice__practice_areas"
        ));
        // A record/reference pick is closed against the database candidate
        // list where the pick resolves, not against YAML choices.
        assert!(!stores_one_declared_choice("country__of_birth"));
        assert!(!stores_one_declared_choice("person__client"));
    }

    #[test]
    fn ensure_declared_choice_checks_every_selected_key_for_a_multiple_choice_state() {
        use super::ensure_declared_choice;
        let state = "custom_multiple_choice__practice_areas";
        let mut declared = BTreeMap::new();
        // Keyed by the role suffix (`metadata_lookup`/`role_key_for_state`),
        // not the full state name — the same key `choices.<key>`
        // uses in template frontmatter.
        declared.insert(
            "practice_areas".to_string(),
            BTreeMap::from([
                ("estate".to_string(), "Estate planning".to_string()),
                ("litigation".to_string(), "Litigation".to_string()),
            ]),
        );
        // Every selection declared: accepted.
        assert!(ensure_declared_choice(&declared, state, r#"["estate","litigation"]"#).is_ok());
        // No selection: a real "chose none of these", accepted.
        assert!(ensure_declared_choice(&declared, state, "[]").is_ok());
        // One undeclared selection among declared ones: the whole answer is
        // refused.
        assert!(
            ensure_declared_choice(&declared, state, r#"["estate","not_a_practice_area"]"#)
                .is_err()
        );
        // Malformed JSON cannot have come from this question's own control.
        assert!(ensure_declared_choice(&declared, state, "estate").is_err());
    }

    #[test]
    fn undeclared_choice_never_carries_the_submitted_value() {
        // The error is returned to callers and logged. A submitted answer is
        // client content, so it must appear in neither the message nor the
        // debug form — only the state and the firm-authored options do.
        let err = NotationSessionError::UndeclaredChoice {
            state: "custom_single_choice__governing_law".to_string(),
            declared: DECLARED_GOVERNING_LAW.map(String::from).to_vec(),
        };
        let shown = format!("{err}");
        let debugged = format!("{err:?}");
        assert!(!shown.contains(OFF_LIST_GOVERNING_LAW), "{shown}");
        assert!(!debugged.contains(OFF_LIST_GOVERNING_LAW), "{debugged}");
        assert!(shown.contains("nevada"), "{shown}");
    }
}
