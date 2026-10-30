//! DBOS port of the `notation` Restate virtual object.
//!
//! The Restate object (`notation_service.rs`) gave one serialized lane per
//! `notation_id`, four K/V values for the two machines' spec and state, and
//! journaled side effects. DBOS has no virtual-object primitive, so the port
//! reproduces the three guarantees with its own pieces:
//!
//! - **The lane** is a PostgreSQL advisory lock held for the whole
//!   transition body (`store::notation_machine_state::acquire_lane`) —
//!   questionnaire and workflow transitions on one notation still serialize,
//!   whichever engine's flag runs them.
//! - **The K/V state** is the `notation_machine_state` row; every state move
//!   is a compare-and-set step (`apply_transition`), so a recovery replay
//!   re-derives the same outcome instead of overwriting a newer one.
//! - **The side effects** run as DBOS steps through the same shared logic the
//!   Restate handlers call (`crate::notation_logic`), so a retry reuses the
//!   checkpoint instead of re-sending mail or appending a second journal row.
//!
//! One DBOS workflow carries exactly one transition (`start`, `signal`, or
//! `back`) and gets a fresh workflow id per call — the parity for Restate
//! giving every POST its own invocation. The caller-facing wire shapes stay
//! the property of the worker ingress (`crate::dbos_ingress`), which speaks
//! the same paths and bodies the Restate ingress did.
//!
//! Store/transport failures are retried by the steps that hit them
//! (`retryable_steps` below); deterministic outcomes — a bad spec, no
//! transition, an unknown notation — surface once as
//! [`NotationTransitionError`] and fail the workflow. That split is the one
//! deliberate divergence from the Restate adapter, which surfaced both
//! classes as terminal; the shared-logic module header records it.

use std::sync::Arc;

use dbos::{StepOptions, WorkflowRef};
use serde::{Deserialize, Serialize};
use store::notation_machine_state::{MachineStateError, TransitionOutcome};
use store::postgres::PostgresDb;
use store::surreal::SurrealDb;
use workflows::{
    dispatch_step, dispatches_side_effect, MachineKind, QuestionnaireSpec, StateName, StepDeps,
    WorkflowSpec,
};

use crate::journal::{answer_payload, append_event, TransitionRecord};
use crate::notation_logic::{
    advance, questionnaire_back_target, record_notation_funnel_step,
    resolve_review_notification_plan, review_notification_hop, review_notification_target,
    send_review_notification, validate_questionnaire_spec, validate_workflow_spec,
    NotationLogicError, ReviewNotificationHop, WorkflowFunnelStep,
};

/// The workflow-specific switch that runs the notation port beside the
/// Restate virtual object. Strict `true`/`false` — a typo must fail boot,
/// not silently leave traffic on Restate.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_NOTATION_ENABLED";

/// Whether [`ENABLED_ENV`] registers the notation transition workflow.
///
/// # Errors
///
/// Refuses any value other than `true` or `false`.
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_NOTATION_ENABLED must be true or false"),
    }
}

/// The DBOS workflow name the transition workflow registers under.
pub const WORKFLOW: &str = "NotationTransition";

/// How many times a retryable step re-runs before the step (and workflow)
/// fails. Restate retried its handlers indefinitely; three bounded attempts
/// with backoff is the DBOS-side parity for the store class — the workflow
/// fails honestly when the store stays down, and the ingress surfaces it.
const STEP_MAX_ATTEMPTS: u32 = 3;

/// Store handles the transition steps need: the `SurrealDB` domain store the
/// journal, funnel, and notification logic read; the PostgreSQL pool backing
/// machine state, lanes, and DBOS itself; and the email/storage services the
/// `email_send__*` / `generate_pdf__*` steps dispatch through.
#[derive(Clone)]
pub struct Context {
    surreal: SurrealDb,
    postgres: PostgresDb,
    email: Arc<dyn workflows::EmailService>,
    storage: Arc<dyn cloud::StorageService>,
}

impl Context {
    #[must_use]
    pub fn new(
        surreal: SurrealDb,
        postgres: PostgresDb,
        email: Arc<dyn workflows::EmailService>,
        storage: Arc<dyn cloud::StorageService>,
    ) -> Self {
        Self {
            surreal,
            postgres,
            email,
            storage,
        }
    }
}

/// One transition request: which machine, which operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionRequest {
    pub kind: MachineKind,
    pub notation_id: uuid::Uuid,
    pub operation: Operation,
}

/// The operation a transition carries — the three mutation handlers the
/// Restate object exposed, in one sum. The reads (`current_state`, `events`)
/// are not transitions; the ingress serves them straight from the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Operation {
    /// `*_start` — validate the spec and begin the machine at `BEGIN`.
    /// Idempotent: an existing machine is left untouched.
    Start {
        spec_yaml: String,
        /// Ephemeral machines have no `notations` row; they skip the
        /// journal. Mirrors `StartBody.ephemeral`.
        #[serde(default)]
        ephemeral: bool,
    },
    /// `*_signal` — advance one condition, journaling the transition and
    /// running the step-kind side effects around it.
    Signal {
        condition: String,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        acting_person_id: Option<uuid::Uuid>,
        #[serde(default)]
        ephemeral: bool,
    },
    /// `questionnaire_back` — move back one answerable question, appending
    /// the immutable `back` journal transition.
    Back { acting_person_id: uuid::Uuid },
}

/// The transition's result: the state the machine is in now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionReport {
    pub next_state: String,
}

/// Why a notation transition failed. Deterministic classes are
/// caller-visible and never retried; `Store` marks a transport failure the
/// steps' retry policy re-runs.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum NotationTransitionError {
    /// A deterministic outcome a replay would reproduce — the caller sees
    /// the failure, no engine retries it.
    #[error("{0}")]
    Terminal(String),
    /// A transition was attempted against a machine that never started.
    #[error("notation machine not started: {notation_id} {machine_kind}")]
    MachineMissing {
        notation_id: uuid::Uuid,
        machine_kind: String,
    },
    /// Another holder kept the notation's lane for the wait budget. The
    /// ingress retries the whole call before surfacing it.
    #[error("notation lane is busy: {notation_id}")]
    LaneBusy { notation_id: uuid::Uuid },
    /// A store or transport failure that a retry on fresh infrastructure
    /// may clear.
    #[error("{0}")]
    Store(String),
}

impl NotationTransitionError {
    /// Whether the steps' retry policy should re-run a failure of this
    /// class. Deterministic outcomes are not worth re-running: a bad spec
    /// is bad on every attempt.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Store(_) | Self::LaneBusy { .. })
    }

    fn from_logic(error: NotationLogicError) -> Self {
        match error {
            NotationLogicError::Terminal(message) => Self::Terminal(message),
            NotationLogicError::Store(message) => Self::Store(message),
        }
    }
}

/// The step retry policy: deterministic classes fail the step immediately,
/// store/transport failures re-run up to [`STEP_MAX_ATTEMPTS`].
fn retryable_steps() -> StepOptions<NotationTransitionError> {
    StepOptions {
        max_attempts: STEP_MAX_ATTEMPTS,
        should_retry: Some(Arc::new(
            |error: &dbos::Error<NotationTransitionError>| match error {
                dbos::Error::Application(application) => application.is_retryable(),
                _ => false,
            },
        )),
        ..StepOptions::default()
    }
}

/// Map a machine-state store failure to the port's error class.
fn store_error(error: MachineStateError) -> NotationTransitionError {
    match error {
        MachineStateError::MachineMissing {
            notation_id,
            machine_kind,
        } => NotationTransitionError::MachineMissing {
            notation_id,
            machine_kind,
        },
        other => NotationTransitionError::Store(other.to_string()),
    }
}

/// The `machine_kind` column value for a machine — the same lowercase token
/// the journal rows and the CHECK constraint use.
fn kind_token(kind: MachineKind) -> &'static str {
    match kind {
        MachineKind::Questionnaire => store::notation_events::MACHINE_QUESTIONNAIRE,
        MachineKind::Workflow => store::notation_events::MACHINE_WORKFLOW,
    }
}

/// A machine's spec, parsed — the two machines keep their own spec types so
/// a bad questionnaire spec names itself, while the advance rules see one
/// [`WorkflowSpec`].
enum ParsedSpec {
    Questionnaire(QuestionnaireSpec),
    Workflow(WorkflowSpec),
}

impl ParsedSpec {
    fn inner(&self) -> &WorkflowSpec {
        match self {
            Self::Questionnaire(spec) => spec.inner(),
            Self::Workflow(spec) => spec,
        }
    }

    /// The transitions leaving one state, if the spec declares any — the
    /// back-completeness check reads it.
    fn transitions_from(&self, state: &StateName) -> Option<&workflows::TransitionMap> {
        self.inner().transitions_from(state)
    }
}

/// Parse a machine's spec — the same round-trip the handlers do before
/// advancing. A parse failure is deterministic (the stored spec never
/// changes), so it maps to [`NotationTransitionError::Terminal`].
fn parse_spec(kind: MachineKind, spec_yaml: &str) -> Result<ParsedSpec, NotationTransitionError> {
    match kind {
        MachineKind::Questionnaire => QuestionnaireSpec::from_yaml(spec_yaml)
            .map(ParsedSpec::Questionnaire)
            .map_err(|error| NotationTransitionError::Terminal(format!("spec: {error}"))),
        MachineKind::Workflow => WorkflowSpec::from_yaml(spec_yaml)
            .map(ParsedSpec::Workflow)
            .map_err(|error| NotationTransitionError::Terminal(format!("spec: {error}"))),
    }
}

pub type NotationTransitionWorkflow =
    WorkflowRef<TransitionRequest, TransitionReport, NotationTransitionError>;

/// Register the transition workflow on a DBOS executor.
///
/// # Errors
///
/// [`dbos::Error`] when the executor rejects the registration.
pub fn register(dbos: &dbos::DBOS, context: Context) -> dbos::Result<NotationTransitionWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: TransitionRequest| {
        let context = context.clone();
        async move { transition(context, request).await }
    })
}

/// One durable notation transition — the port of the three mutation
/// handlers. The lane is taken in the body (re-taken on recovery), every
/// side effect is a checkpointed step, and the guard's rollback releases
/// the lane on any failure path.
async fn transition(
    context: Context,
    request: TransitionRequest,
) -> dbos::Result<TransitionReport, NotationTransitionError> {
    let notation_id = request.notation_id;

    // The lane: one advisory lock per notation, held across the whole
    // transition. Re-acquired on recovery; the guard's rollback releases it
    // even on a panic path. A wait-budget expiry fails the call with
    // `LaneBusy` — no step has run yet, so the ingress retry is safe.
    let _lane = store::notation_machine_state::acquire_lane(&context.postgres, notation_id)
        .await
        .map_err(|error| match error {
            MachineStateError::LaneTimeout { .. } => {
                dbos::Error::Application(NotationTransitionError::LaneBusy { notation_id })
            }
            other => dbos::Error::Application(NotationTransitionError::Store(other.to_string())),
        })?;

    match request.operation {
        Operation::Start {
            spec_yaml,
            ephemeral,
        } => start_machine(&context, request.kind, notation_id, &spec_yaml, ephemeral).await,
        Operation::Signal {
            condition,
            value,
            acting_person_id,
            ephemeral,
        } => {
            signal(
                &context,
                request.kind,
                notation_id,
                &condition,
                value,
                acting_person_id,
                ephemeral,
            )
            .await
        }
        Operation::Back { acting_person_id } => {
            back(
                &context,
                notation_id,
                kind_token(request.kind),
                acting_person_id,
            )
            .await
        }
    }
}

/// Read one machine row as a durable step — the parity of the Restate K/V
/// get. `MachineMissing` is deterministic: the caller signaled a machine
/// that never started.
async fn read_machine(
    context: &Context,
    notation_id: uuid::Uuid,
    kind: MachineKind,
) -> dbos::Result<store::notation_machine_state::MachineState, NotationTransitionError> {
    let token = kind_token(kind).to_owned();
    dbos::step_with("read-machine", retryable_steps(), move || {
        let postgres = context.postgres.clone();
        let token = token.clone();
        async move {
            store::notation_machine_state::read(&postgres, notation_id, &token)
                .await
                .map_err(|error| dbos::Error::Application(store_error(error)))?
                .ok_or_else(|| {
                    dbos::Error::Application(NotationTransitionError::MachineMissing {
                        notation_id,
                        machine_kind: token.clone(),
                    })
                })
        }
    })
    .await
}

/// The compare-and-set state move as a durable step: `Applied` on the first
/// run, `AlreadyApplied` on a recovery replay whose transition landed before
/// the crash, `Conflicted` when another invocation moved the machine past
/// `from` — the machine is consistent, this zombie call just loses.
async fn apply_transition_step(
    context: &Context,
    notation_id: uuid::Uuid,
    token: &str,
    from: &str,
    next: &str,
) -> dbos::Result<(), NotationTransitionError> {
    let token = token.to_owned();
    let from = from.to_owned();
    let next = next.to_owned();
    dbos::step_with("apply-transition", retryable_steps(), move || {
        let postgres = context.postgres.clone();
        let token = token.clone();
        let from = from.clone();
        let next = next.clone();
        async move {
            match store::notation_machine_state::apply_transition(
                &postgres,
                notation_id,
                &token,
                &from,
                &next,
            )
            .await
            {
                Ok(TransitionOutcome::Applied | TransitionOutcome::AlreadyApplied) => Ok(()),
                Ok(TransitionOutcome::Conflicted { current }) => Err(dbos::Error::Application(
                    NotationTransitionError::Terminal(format!(
                        "notation {notation_id} moved past `{from}` — it is in `{current}`"
                    )),
                )),
                Err(error) => Err(dbos::Error::Application(store_error(error))),
            }
        }
    })
    .await
}

/// One funnel event as a durable step (skipped for ephemeral machines by the
/// callers — the funnel reads the `notations` row).
async fn record_funnel_step(
    context: &Context,
    notation_id: uuid::Uuid,
    step: WorkflowFunnelStep,
) -> dbos::Result<(), NotationTransitionError> {
    dbos::step_with("record-funnel-step", retryable_steps(), move || {
        let surreal = context.surreal.clone();
        async move {
            record_notation_funnel_step(&surreal, notation_id, step)
                .await
                .map_err(NotationTransitionError::from_logic)
                .map_err(dbos::Error::Application)
        }
    })
    .await
}

/// One review notification send as a durable step. The mailbox and Project
/// code are read inside the step, never carried across it: this run's
/// journaled value is `()`, so neither reaches the journal.
async fn send_notification_step(
    context: &Context,
    notation_id: uuid::Uuid,
    project_id: uuid::Uuid,
    recipient_id: uuid::Uuid,
    hop: ReviewNotificationHop,
) -> dbos::Result<(), NotationTransitionError> {
    dbos::step_with("send-review-notification", retryable_steps(), move || {
        let surreal = context.surreal.clone();
        let email = Arc::clone(&context.email);
        async move {
            let target = review_notification_target(&surreal, project_id, recipient_id)
                .await
                .map_err(NotationTransitionError::from_logic)
                .map_err(dbos::Error::Application)?;
            send_review_notification(
                Arc::clone(&email),
                notation_id,
                project_id,
                &target.project_code,
                recipient_id,
                &target.address,
                hop,
            )
            .await
            .map_err(NotationTransitionError::from_logic)
            .map_err(dbos::Error::Application)
        }
    })
    .await
}

/// `*_start`: validate the spec, then begin the machine at `BEGIN` if absent.
/// The step is idempotent (`ON CONFLICT DO NOTHING`), so a recovery replay
/// and a duplicate start both land on the same row.
async fn start_machine(
    context: &Context,
    kind: MachineKind,
    notation_id: uuid::Uuid,
    spec_yaml: &str,
    ephemeral: bool,
) -> dbos::Result<TransitionReport, NotationTransitionError> {
    // Validate before storing so a later signal can't fail mid-flight — the
    // parity of the Restate start handlers. Pure and deterministic on the
    // request, so it runs in the body: a bad spec fails without touching
    // the store.
    match kind {
        MachineKind::Questionnaire => validate_questionnaire_spec(spec_yaml),
        MachineKind::Workflow => validate_workflow_spec(spec_yaml),
    }
    .map_err(NotationTransitionError::from_logic)
    .map_err(dbos::Error::Application)?;

    let token = kind_token(kind).to_owned();
    let spec = spec_yaml.to_owned();
    dbos::step_with("start-machine", retryable_steps(), move || {
        let postgres = context.postgres.clone();
        let token = token.clone();
        let spec = spec.clone();
        async move {
            store::notation_machine_state::start_machine(
                &postgres,
                notation_id,
                &token,
                &spec,
                ephemeral,
            )
            .await
            .map_err(|error| dbos::Error::Application(store_error(error)))
        }
    })
    .await?;

    Ok(TransitionReport {
        next_state: StateName::BEGIN.to_string(),
    })
}

/// `*_signal`: read the machine, advance it, and run the side effects in the
/// Restate handler's order — state move first (the CAS), then the step-kind
/// dispatch (its artifact can ride the journal row's payload), then the
/// journal append, then the funnel/notification/close effects. The
/// questionnaire never dispatches: its answer payload rides the journal row
/// directly, exactly as its handler does.
#[allow(clippy::too_many_lines)]
async fn signal(
    context: &Context,
    kind: MachineKind,
    notation_id: uuid::Uuid,
    condition: &str,
    value: Option<String>,
    acting_person_id: Option<uuid::Uuid>,
    ephemeral: bool,
) -> dbos::Result<TransitionReport, NotationTransitionError> {
    let token = kind_token(kind);
    let machine = read_machine(context, notation_id, kind).await?;

    // Parse and advance — pure, deterministic on the row just read.
    let spec = parse_spec(kind, &machine.spec_yaml).map_err(dbos::Error::Application)?;
    let from_state = StateName::from(machine.current_state.as_str());
    let next = advance(spec.inner(), &from_state, condition)
        .map_err(NotationTransitionError::from_logic)
        .map_err(dbos::Error::Application)?;

    apply_transition_step(
        context,
        notation_id,
        token,
        &machine.current_state,
        next.as_str(),
    )
    .await?;

    // Step-kind side effects run BEFORE the journal append so the artifact
    // they produce can ride the journal row's payload — the workflow
    // handler's order, unchanged. The questionnaire never dispatches.
    let dispatch_payload: Option<String> =
        if kind == MachineKind::Workflow && dispatches_side_effect(&next) {
            let email = Arc::clone(&context.email);
            let storage = Arc::clone(&context.storage);
            let surreal = context.surreal.clone();
            let state = next.clone();
            let value = value.clone();
            dbos::step_with("dispatch-step", retryable_steps(), move || {
                let email = Arc::clone(&email);
                let storage = Arc::clone(&storage);
                let surreal = surreal.clone();
                let state = state.clone();
                let value = value.clone();
                async move {
                    let deps = StepDeps::new(email, storage).with_surreal(surreal);
                    dispatch_step(&deps, notation_id, &state, value.as_deref())
                        .await
                        .map_err(|error| {
                            dbos::Error::Application(NotationTransitionError::Store(format!(
                                "dispatch: {error}"
                            )))
                        })
                }
            })
            .await?
        } else {
            None
        };

    // The journal append — skipped for ephemeral machines, which have no
    // `notations` row for `append_event`'s read-back. A questionnaire signal
    // journals its answer payload; a workflow signal journals the step's
    // artifact when it produced one.
    if !ephemeral {
        let journal_payload = match kind {
            MachineKind::Questionnaire => value.as_deref().map(answer_payload),
            MachineKind::Workflow => dispatch_payload,
        };
        let surreal = context.surreal.clone();
        let from_str = machine.current_state.clone();
        let to_str = next.as_str().to_owned();
        let condition_for_step = condition.to_owned();
        dbos::step_with("append-event", retryable_steps(), move || {
            let surreal = surreal.clone();
            let from_str = from_str.clone();
            let to_str = to_str.clone();
            let condition = condition_for_step.clone();
            let payload = journal_payload.clone();
            async move {
                let recorded_at = chrono::Utc::now().to_rfc3339();
                append_event(
                    &surreal,
                    TransitionRecord {
                        notation_id,
                        acting_person_id,
                        machine_kind: kind_token(kind),
                        from_state: &from_str,
                        to_state: &to_str,
                        condition: &condition,
                        payload_json: payload,
                        recorded_at: &recorded_at,
                    },
                )
                .await
                .map_err(|error| {
                    dbos::Error::Application(NotationTransitionError::Store(format!(
                        "journal: {error}"
                    )))
                })
                .map(|_| ())
            }
        })
        .await?;

        if let Some(funnel_step) =
            crate::notation_logic::workflow_funnel_step(&from_state, condition, &next)
        {
            record_funnel_step(context, notation_id, funnel_step).await?;
        }

        // A notification follows the record of the transition it announces.
        if let Some(hop) = review_notification_hop(&from_state, condition, &next) {
            let plan = dbos::step_with(
                "resolve-review-notification-recipients",
                retryable_steps(),
                move || {
                    let surreal = context.surreal.clone();
                    async move {
                        resolve_review_notification_plan(&surreal, notation_id, hop)
                            .await
                            .map_err(NotationTransitionError::from_logic)
                            .map_err(dbos::Error::Application)
                    }
                },
            )
            .await?;
            let record_client_handoff =
                matches!(hop, ReviewNotificationHop::Client) && !plan.recipient_ids.is_empty();

            for recipient_id in plan.recipient_ids.iter().copied() {
                send_notification_step(context, notation_id, plan.project_id, recipient_id, hop)
                    .await?;
            }

            if record_client_handoff {
                record_funnel_step(
                    context,
                    notation_id,
                    WorkflowFunnelStep::Sent(telemetry::FunnelChannel::Email),
                )
                .await?;
            }
        }

        // The firm signing the closing letter closes the matter — the
        // symmetric bookend to the client-signed retainer that opened it.
        if workflows::closes_matter(&from_state) {
            let surreal = context.surreal.clone();
            dbos::step_with("close-matter", retryable_steps(), move || {
                let surreal = surreal.clone();
                async move {
                    workflows::close_matter(&surreal, notation_id)
                        .await
                        .map_err(|error| {
                            dbos::Error::Application(NotationTransitionError::Store(format!(
                                "close: {error}"
                            )))
                        })
                }
            })
            .await?;
        }
    }

    Ok(TransitionReport {
        next_state: next.as_str().to_owned(),
    })
}

/// `questionnaire_back`: move back one answerable question, appending the
/// immutable `back` transition. The first question, a completed
/// questionnaire, and the terminal state all stay put.
async fn back(
    context: &Context,
    notation_id: uuid::Uuid,
    token: &str,
    acting_person_id: uuid::Uuid,
) -> dbos::Result<TransitionReport, NotationTransitionError> {
    let machine = read_machine(context, notation_id, MachineKind::Questionnaire).await?;

    // A completed questionnaire stays put — the same guard the handler
    // applies, deterministic on the row just read.
    let current_state = StateName::from(machine.current_state.as_str());
    let spec = parse_spec(MachineKind::Questionnaire, &machine.spec_yaml)
        .map_err(dbos::Error::Application)?;
    let questionnaire_complete = spec
        .transitions_from(&current_state)
        .and_then(|transitions| transitions.lookup("_"))
        == Some(&StateName::end());

    if current_state == StateName::begin()
        || current_state == StateName::end()
        || questionnaire_complete
    {
        return Ok(TransitionReport {
            next_state: machine.current_state,
        });
    }

    // One step reads the journal, picks the back target, and appends the
    // immutable event — the Restate handler's single journaled unit. A
    // missing target means "stay put": no event, state unchanged.
    let token_owned = token.to_owned();
    let from = machine.current_state.clone();
    let previous = dbos::step_with("append-back", retryable_steps(), move || {
        let surreal = context.surreal.clone();
        let token = token_owned.clone();
        let from = from.clone();
        async move {
            let events = store::notation_events::for_notation(&surreal, notation_id)
                .await
                .map_err(|error| {
                    dbos::Error::Application(NotationTransitionError::Store(format!(
                        "journal: {error}"
                    )))
                })?;
            let history: Vec<_> = events
                .into_iter()
                .filter(|event| event.machine_kind == token)
                .map(|event| {
                    (
                        event.from_state,
                        event.to_state,
                        event.condition,
                        event.id.to_string(),
                    )
                })
                .collect();
            let Some(previous) =
                questionnaire_back_target(&StateName::from(from.as_str()), &history)
            else {
                return Ok(from.clone());
            };
            let previous = previous.as_str().to_owned();
            let recorded_at = chrono::Utc::now().to_rfc3339();
            append_event(
                &surreal,
                TransitionRecord {
                    notation_id,
                    acting_person_id: Some(acting_person_id),
                    machine_kind: &token,
                    from_state: &from,
                    to_state: &previous,
                    condition: "back",
                    payload_json: None,
                    recorded_at: &recorded_at,
                },
            )
            .await
            .map_err(|error| {
                dbos::Error::Application(NotationTransitionError::Store(format!(
                    "journal: {error}"
                )))
            })?;
            Ok(previous)
        }
    })
    .await?;

    if previous != machine.current_state {
        apply_transition_step(
            context,
            notation_id,
            token,
            &machine.current_state,
            &previous,
        )
        .await?;
    }

    Ok(TransitionReport {
        next_state: previous,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        enabled, kind_token, parse_spec, retryable_steps, NotationTransitionError, Operation,
        TransitionRequest, ENABLED_ENV,
    };
    use store::notation_events::{MACHINE_QUESTIONNAIRE, MACHINE_WORKFLOW};
    use workflows::MachineKind;

    use crate::notation_logic::NotationLogicError;

    const WORKFLOW_SPEC: &str = "
BEGIN:
  created: END
END: {}
";

    const QUESTIONNAIRE_SPEC: &str = "
BEGIN:
  _: END
END: {}
";

    #[test]
    fn flag_follows_the_strict_switch_contract() {
        assert!(!enabled(|_| None).expect("unset is off"));
        assert!(
            !enabled(|name| (name == ENABLED_ENV).then(|| "false".to_owned()))
                .expect("false is off")
        );
        assert!(!enabled(|name| (name == ENABLED_ENV).then(String::new)).expect("empty is off"));
        assert!(
            enabled(|name| (name == ENABLED_ENV).then(|| "true".to_owned())).expect("true is on")
        );
        assert!(
            enabled(|name| (name == ENABLED_ENV).then(|| "TRUE".to_owned())).is_err(),
            "non-lowercase truthy values fail closed, like every other DBOS flag"
        );
    }

    #[test]
    fn retry_policy_retries_store_failures_and_deterministic_outcomes_fail_fast() {
        let options = retryable_steps();
        let should_retry = options.should_retry.expect("the policy sets a predicate");
        let retryable =
            |error: NotationTransitionError| should_retry(&dbos::Error::Application(error));
        assert!(retryable(NotationTransitionError::Store("db down".into())));
        assert!(retryable(NotationTransitionError::LaneBusy {
            notation_id: uuid::Uuid::now_v7(),
        }));
        assert!(!retryable(NotationTransitionError::Terminal(
            "bad spec".into()
        )));
        assert!(!retryable(NotationTransitionError::MachineMissing {
            notation_id: uuid::Uuid::now_v7(),
            machine_kind: "workflow".into(),
        }));
    }

    #[test]
    fn parse_spec_keeps_the_two_machine_kinds_distinct() {
        let questionnaire = parse_spec(MachineKind::Questionnaire, QUESTIONNAIRE_SPEC)
            .expect("valid questionnaire spec");
        assert!(questionnaire
            .inner()
            .transitions_from(&workflows::StateName::begin())
            .is_some());

        let workflow =
            parse_spec(MachineKind::Workflow, WORKFLOW_SPEC).expect("valid workflow spec");
        assert!(workflow
            .inner()
            .transitions_from(&workflows::StateName::begin())
            .is_some());

        let garbage = parse_spec(MachineKind::Questionnaire, "not: [a: spec");
        assert!(
            matches!(garbage, Err(NotationTransitionError::Terminal(_))),
            "garbage YAML is a deterministic failure, not a retry"
        );
    }

    #[test]
    fn kind_tokens_match_the_journal_and_check_constraint() {
        assert_eq!(
            kind_token(MachineKind::Questionnaire),
            MACHINE_QUESTIONNAIRE
        );
        assert_eq!(kind_token(MachineKind::Workflow), MACHINE_WORKFLOW);
    }

    #[test]
    fn transition_requests_round_trip_through_the_checkpoint_format() {
        let notation = uuid::Uuid::now_v7();
        let request = TransitionRequest {
            kind: MachineKind::Questionnaire,
            notation_id: notation,
            operation: Operation::Signal {
                condition: "_".to_owned(),
                value: Some("Libra".to_owned()),
                acting_person_id: Some(uuid::Uuid::now_v7()),
                ephemeral: false,
            },
        };
        let encoded = serde_json::to_vec(&request).expect("serialize");
        let decoded: TransitionRequest = serde_json::from_slice(&encoded).expect("deserialize");
        assert_eq!(decoded, request);
        assert!(
            matches!(decoded.operation, Operation::Signal { ref condition, .. } if condition == "_")
        );
    }

    #[test]
    fn logic_failures_map_to_the_port_error_classes() {
        let terminal = NotationTransitionError::from_logic(NotationLogicError::Terminal(
            "no such state".into(),
        ));
        assert!(matches!(terminal, NotationTransitionError::Terminal(_)));
        assert!(!terminal.is_retryable());

        let store =
            NotationTransitionError::from_logic(NotationLogicError::Store("surreal down".into()));
        assert!(matches!(store, NotationTransitionError::Store(_)));
        assert!(store.is_retryable());
    }
}
