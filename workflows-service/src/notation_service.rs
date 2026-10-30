//! Restate virtual-object service exposing the two state-machine
//! timelines per Notation.
//!
//! One Restate virtual object per Notation (the object key is the
//! stringified `notation_id`). Hosts both machine kinds under the
//! same key so signals serialize on a single logical journal —
//! questionnaire and workflow handlers can never interleave for
//! the same Notation.
//!
//! Each `*_signal` handler:
//!   1. Reads the stored spec yaml + current state from Restate's
//!      keyed state (`ctx.get`).
//!   2. Computes the next state by parsing the spec and looking
//!      up the transition.
//!   3. Writes the new state back (`ctx.set`).
//!   4. Inside `ctx.run("append-event", …)`, appends one row to
//!      the `notation_events` journal (`SurrealDB`).
//!
//! The `ctx.run` wrapper is the load-bearing trick: it makes the
//! journal write a Restate-journaled side effect, so a replay
//! reuses the cached row id instead of double-writing.

use std::sync::Arc;

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use tracing::Instrument;
use workflows::{
    dispatch_step, dispatches_side_effect, EmailService, MachineKind, QuestionnaireSpec, StateName,
    StepDeps, WorkflowSpec,
};

use crate::journal::{answer_payload, append_event, TransitionRecord};
use crate::notation_logic::{
    advance, questionnaire_back_target, record_notation_funnel_step,
    resolve_review_notification_plan, review_notification_hop, review_notification_target,
    send_review_notification, validate_questionnaire_spec, validate_workflow_spec,
    workflow_funnel_step, NotationLogicError, ReviewNotificationHop, WorkflowFunnelStep,
};
#[cfg(test)]
use crate::notation_logic::{review_copy, review_recipient_ids};

/// Build a handler span parented on the W3C trace context the caller injected
/// into the ingress POST (see `telemetry`), extracted from this invocation's
/// headers. Lets a `web`-initiated workflow and its durable steps render as one
/// trace. Shared by every Notation handler; a no-op parent (fresh root) when no
/// `traceparent` is present (dev / KIND / OSS forks). `key` is the opaque
/// notation id — an allow-listed id, never client content.
fn traced_handler_span(handler: &'static str, headers: &HeaderMap, key: &str) -> tracing::Span {
    let span = tracing::info_span!(
        "notation.handler",
        handler = handler,
        key = %key,
        navigator.keep = true,
    );
    telemetry::set_span_parent(
        &span,
        headers.get("traceparent").map(String::as_str),
        headers.get("tracestate").map(String::as_str),
    );
    span
}

/// Restate state key for the questionnaire spec yaml.
const QUESTIONNAIRE_SPEC_KEY: &str = "questionnaire_spec_yaml";
/// Restate state key for the questionnaire's current state name.
const QUESTIONNAIRE_STATE_KEY: &str = "questionnaire_state";
/// Restate state key for the workflow spec yaml.
const WORKFLOW_SPEC_KEY: &str = "workflow_spec_yaml";
/// Restate state key for the workflow's current state name.
const WORKFLOW_STATE_KEY: &str = "workflow_state";

/// JSON request body for a `*_start` handler.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StartBody {
    pub spec_yaml: String,
    /// `true` for workflows that have no `notations` row (e.g. the
    /// `onboarding__welcome` trigger). The signal handler skips the
    /// `notation_events` journal append for these, since `append_event`
    /// would fail reading back a notation that does not exist. Defaults
    /// to `false` for legacy callers that always start a
    /// notation-backed workflow.
    #[serde(default)]
    pub ephemeral: bool,
}

/// JSON request body for `questionnaire_signal`. The `value`
/// becomes the `answer_value` payload of the journaled event.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuestionnaireSignalBody {
    pub condition: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub acting_person_id: Option<uuid::Uuid>,
}

/// The actor who is moving the questionnaire back one answerable state.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuestionnaireBackBody {
    pub acting_person_id: uuid::Uuid,
}

/// JSON request body for `workflow_signal`. The optional `value`
/// carries a JSON-serialized payload for step dispatch: `email_send__*`
/// reads it as a [`workflows::EmailPayload`] and `generate_pdf__*`
/// reads it as a [`workflows::DocumentPayload`]; other step kinds
/// ignore it. `ephemeral` propagates the flag set at start time so each
/// signal also skips the journal — required because the worker doesn't
/// persist start-side flags across the keyed state today.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WorkflowSignalBody {
    pub condition: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub acting_person_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub ephemeral: bool,
}

/// JSON response body for a `*_signal` handler.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SignalResponse {
    pub next_state: String,
}

/// JSON response body for `*_current_state`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CurrentStateResponse {
    pub state: Option<String>,
}

/// Service struct held by the Restate endpoint. Carries the shared
/// store handle, the worker-side [`workflows::EmailService`] that
/// `email_send__*` step dispatch routes through, and the
/// [`cloud::StorageService`] that `generate_pdf__*` step dispatch
/// persists rendered PDFs to.
#[derive(Clone)]
pub struct NotationService {
    surreal: store::surreal::SurrealDb,
    email: Arc<dyn EmailService>,
    storage: Arc<dyn cloud::StorageService>,
}

impl NotationService {
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        email: Arc<dyn EmailService>,
        storage: Arc<dyn cloud::StorageService>,
    ) -> Self {
        Self {
            surreal,
            email,
            storage,
        }
    }
}

/// Parse the Restate object key into a notation id.
fn parse_notation_id(key: &str) -> Result<uuid::Uuid, HandlerError> {
    key.parse::<uuid::Uuid>().map_err(|e| {
        HandlerError::from(TerminalError::new(format!(
            "object key `{key}` is not a valid notation_id: {e}"
        )))
    })
}

#[restate_sdk::object(name = "notation")]
impl NotationService {
    /// Begin the questionnaire walk for this Notation. Idempotent
    /// — if the spec is already stored, this is a no-op.
    #[restate_sdk::handler]
    async fn questionnaire_start(
        &self,
        ctx: ObjectContext<'_>,
        body: Json<StartBody>,
    ) -> Result<(), HandlerError> {
        let span = traced_handler_span("questionnaire_start", ctx.headers(), ctx.key());
        async move {
            let body = body.0;
            if ctx.get::<String>(QUESTIONNAIRE_SPEC_KEY).await?.is_some() {
                return Ok(()); // idempotent
            }
            // Validate the yaml round-trips before storing so a later
            // signal can't fail mid-flight.
            validate_questionnaire_spec(&body.spec_yaml).map_err(terminal_from)?;
            ctx.set(QUESTIONNAIRE_SPEC_KEY, body.spec_yaml);
            ctx.set(QUESTIONNAIRE_STATE_KEY, StateName::BEGIN.to_string());
            Ok(())
        }
        .instrument(span)
        .await
    }

    /// Advance the questionnaire one step.
    #[restate_sdk::handler]
    async fn questionnaire_signal(
        &self,
        ctx: ObjectContext<'_>,
        body: Json<QuestionnaireSignalBody>,
    ) -> Result<Json<SignalResponse>, HandlerError> {
        let span = traced_handler_span("questionnaire_signal", ctx.headers(), ctx.key());
        async move {
            let body = body.0;
            let notation_id = parse_notation_id(ctx.key())?;
            let spec_yaml: String = ctx
                .get::<String>(QUESTIONNAIRE_SPEC_KEY)
                .await?
                .ok_or_else(|| TerminalError::new("questionnaire has not been started"))?;
            let spec = QuestionnaireSpec::from_yaml(&spec_yaml)
                .map_err(|e| TerminalError::new(format!("spec: {e}")))?;
            let from = ctx
                .get::<String>(QUESTIONNAIRE_STATE_KEY)
                .await?
                .unwrap_or_else(|| StateName::BEGIN.to_string());
            let from_state = StateName::from(from.as_str());
            let next = next_state(spec.inner(), &from_state, &body.condition)?;

            ctx.set(QUESTIONNAIRE_STATE_KEY, next.as_str().to_string());

            let surreal = self.surreal.clone();
            let payload = body.value.as_deref().map(answer_payload);
            let from_str = from.clone();
            let to_str = next.as_str().to_string();
            let condition = body.condition.clone();
            ctx.run(|| async move {
                let recorded_at = chrono::Utc::now().to_rfc3339();
                append_event(
                    &surreal,
                    TransitionRecord {
                        notation_id,
                        acting_person_id: body.acting_person_id,
                        machine_kind: MachineKind::Questionnaire.as_str(),
                        from_state: &from_str,
                        to_state: &to_str,
                        condition: &condition,
                        payload_json: payload,
                        recorded_at: &recorded_at,
                    },
                )
                .await
                .map(|_| ())
                .map_err(|e| HandlerError::from(TerminalError::new(format!("journal: {e}"))))
            })
            .name("append-questionnaire-event")
            .await?;

            if next == StateName::end() {
                let surreal = self.surreal.clone();
                ctx.run(move || async move {
                    record_notation_funnel_step(
                        &surreal,
                        notation_id,
                        WorkflowFunnelStep::IntakeComplete,
                    )
                    .await
                    .map_err(terminal_from)
                })
                .name("record-funnel-intake-complete")
                .await?;
            }

            Ok(Json(SignalResponse {
                next_state: next.as_str().to_string(),
            }))
        }
        .instrument(span)
        .await
    }

    /// Move back one answerable question, appending an immutable journal
    /// transition. A retry reuses the `ctx.run` result instead of appending a
    /// second event, and the first question or completed questionnaire stays
    /// put.
    #[restate_sdk::handler]
    async fn questionnaire_back(
        &self,
        ctx: ObjectContext<'_>,
        body: Json<QuestionnaireBackBody>,
    ) -> Result<Json<SignalResponse>, HandlerError> {
        let span = traced_handler_span("questionnaire_back", ctx.headers(), ctx.key());
        async move {
            let notation_id = parse_notation_id(ctx.key())?;
            let body = body.0;
            let current = ctx
                .get::<String>(QUESTIONNAIRE_STATE_KEY)
                .await?
                .unwrap_or_else(|| StateName::BEGIN.to_string());
            let current_state = StateName::from(current.as_str());
            let spec_yaml = ctx.get::<String>(QUESTIONNAIRE_SPEC_KEY).await?;
            let questionnaire_complete = spec_yaml
                .map(|yaml| {
                    workflows::spec::QuestionnaireSpec::from_yaml(&yaml)
                        .map_err(|error| {
                            HandlerError::from(TerminalError::new(format!(
                                "invalid questionnaire spec: {error}"
                            )))
                        })
                        .map(|spec| {
                            spec.transitions_from(&current_state)
                                .and_then(|transitions| transitions.lookup("_"))
                                == Some(&StateName::end())
                        })
                })
                .transpose()?
                .unwrap_or(false);
            let next = if current_state == StateName::begin()
                || current_state == StateName::end()
                || questionnaire_complete
            {
                current
            } else {
                let surreal = self.surreal.clone();
                let from = current.clone();
                ctx.run(|| async move {
                    let events = store::notation_events::for_notation(&surreal, notation_id)
                        .await
                        .map_err(|error| {
                            HandlerError::from(TerminalError::new(format!("journal: {error}")))
                        })?;
                    let history: Vec<_> = events
                        .into_iter()
                        .filter(|event| event.machine_kind == MachineKind::Questionnaire.as_str())
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
                        return Ok(from);
                    };
                    let previous = previous.as_str().to_string();
                    let recorded_at = chrono::Utc::now().to_rfc3339();
                    append_event(
                        &surreal,
                        TransitionRecord {
                            notation_id,
                            acting_person_id: Some(body.acting_person_id),
                            machine_kind: MachineKind::Questionnaire.as_str(),
                            from_state: &from,
                            to_state: &previous,
                            condition: "back",
                            payload_json: None,
                            recorded_at: &recorded_at,
                        },
                    )
                    .await
                    .map_err(|error| {
                        HandlerError::from(TerminalError::new(format!("journal: {error}")))
                    })?;
                    Ok(previous)
                })
                .name("append-questionnaire-back")
                .await?
            };
            ctx.set(QUESTIONNAIRE_STATE_KEY, next.clone());
            Ok(Json(SignalResponse { next_state: next }))
        }
        .instrument(span)
        .await
    }

    #[restate_sdk::handler]
    async fn questionnaire_current_state(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<CurrentStateResponse>, HandlerError> {
        let span = traced_handler_span("questionnaire_current_state", ctx.headers(), ctx.key());
        async move {
            let state = ctx.get::<String>(QUESTIONNAIRE_STATE_KEY).await?;
            Ok(Json(CurrentStateResponse { state }))
        }
        .instrument(span)
        .await
    }

    /// Begin the post-intake workflow for this Notation. Idempotent.
    #[restate_sdk::handler]
    async fn workflow_start(
        &self,
        ctx: ObjectContext<'_>,
        body: Json<StartBody>,
    ) -> Result<(), HandlerError> {
        let span = traced_handler_span("workflow_start", ctx.headers(), ctx.key());
        async move {
            let body = body.0;
            if ctx.get::<String>(WORKFLOW_SPEC_KEY).await?.is_some() {
                return Ok(()); // idempotent
            }
            validate_workflow_spec(&body.spec_yaml).map_err(terminal_from)?;
            ctx.set(WORKFLOW_SPEC_KEY, body.spec_yaml);
            ctx.set(WORKFLOW_STATE_KEY, StateName::BEGIN.to_string());
            Ok(())
        }
        .instrument(span)
        .await
    }

    /// Advance the workflow one step.
    #[restate_sdk::handler]
    #[allow(clippy::too_many_lines)]
    async fn workflow_signal(
        &self,
        ctx: ObjectContext<'_>,
        body: Json<WorkflowSignalBody>,
    ) -> Result<Json<SignalResponse>, HandlerError> {
        let span = traced_handler_span("workflow_signal", ctx.headers(), ctx.key());
        async move {
            let body = body.0;
            let notation_id = parse_notation_id(ctx.key())?;
            let spec_yaml: String = ctx
                .get::<String>(WORKFLOW_SPEC_KEY)
                .await?
                .ok_or_else(|| TerminalError::new("workflow has not been started"))?;
            let spec = WorkflowSpec::from_yaml(&spec_yaml)
                .map_err(|e| TerminalError::new(format!("spec: {e}")))?;
            let from = ctx
                .get::<String>(WORKFLOW_STATE_KEY)
                .await?
                .unwrap_or_else(|| StateName::BEGIN.to_string());
            let from_state = StateName::from(from.as_str());
            let next = next_state(&spec, &from_state, &body.condition)?;

            ctx.set(WORKFLOW_STATE_KEY, next.as_str().to_string());

            // Step-kind side effects, routed through the one shared
            // `workflows::dispatch_step` registry so the prod worker and the
            // in-process dev/BDD runtime run the exact same dispatch arm —
            // `email_send__*` (SendGrid), `generate_pdf__*` (render + GCS
            // persist + blob/`documents` file), and the submission steps
            // `mailroom_send` / `certified_mail` / `e_filing` / `filing__*`
            // (a durable `filings` row, guaranteed `lawyer_review`-gated by
            // the spec).
            //
            // The side effect runs BEFORE the journal append so a step that
            // produces a durable artifact (the `generate_pdf` blob ref) can
            // record it on the transition's own `notation_events.payload` —
            // set at append time, honoring the append-only journal (rows are
            // never updated). The `ctx.run` boundary stays here, outside the
            // registry, so the effect is journaled and a replay reuses the
            // cached outcome rather than re-emailing / double-filing /
            // double-writing. We only open the journal entry when the step
            // actually dispatches, so human/wait states (`lawyer_review`,
            // `_signature`, …) stay dispatch-free as before.
            let dispatch_payload: Option<String> = if dispatches_side_effect(&next) {
                let deps = StepDeps::new(Arc::clone(&self.email), Arc::clone(&self.storage))
                    // `assets` and `templates` moved to SurrealDB with ENG-121:
                    // `document_intake` files its row and `generate_pdf` reads
                    // the pinned template's declared kind through this handle.
                    .with_surreal(self.surreal.clone());
                let state = next.clone();
                let value = body.value.clone();
                ctx.run(|| async move {
                    dispatch_workflow_step(&deps, notation_id, &state, value.as_deref()).await
                })
                .name("dispatch-step")
                .await?
            } else {
                None
            };

            // Ephemeral workflows (e.g. `onboarding__welcome`) have no
            // `notations` row, so `append_event`'s read-back of the
            // notation (`notations` moved to SurrealDB with ENG-121) would
            // fail. Skip the journal in that case; the durability for
            // ephemeral steps lives downstream (the `sent_emails` audit row
            // for `email_send__*` dispatch).
            if !body.ephemeral {
                let surreal = self.surreal.clone();
                let from_str = from.clone();
                let to_str = next.as_str().to_string();
                let condition = body.condition.clone();
                let acting_person_id = body.acting_person_id;
                ctx.run(|| async move {
                    let recorded_at = chrono::Utc::now().to_rfc3339();
                    append_event(
                        &surreal,
                        TransitionRecord {
                            notation_id,
                            acting_person_id,
                            machine_kind: MachineKind::Workflow.as_str(),
                            from_state: &from_str,
                            to_state: &to_str,
                            condition: &condition,
                            payload_json: dispatch_payload,
                            recorded_at: &recorded_at,
                        },
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| HandlerError::from(TerminalError::new(format!("journal: {e}"))))
                })
                .name("append-workflow-event")
                .await?;
            }

            if !body.ephemeral {
                if let Some(step) = workflow_funnel_step(&from_state, &body.condition, &next) {
                    let surreal = self.surreal.clone();
                    ctx.run(move || async move {
                        record_notation_funnel_step(&surreal, notation_id, step)
                            .await
                            .map_err(terminal_from)
                    })
                    .name("record-funnel-step")
                    .await?;
                }
            }

            // A notification follows the record of the transition it announces.
            // The only reason anything precedes `append-workflow-event` is a
            // step that produces a payload for that row, and a notification
            // produces none — running it first meant a mail-provider failure
            // left the transition unjournaled.
            if !body.ephemeral {
                if let Some(hop) = review_notification_hop(&from_state, &body.condition, &next) {
                    let surreal = self.surreal.clone();
                    let plan = ctx
                        .run(move || async move {
                            resolve_review_notification_plan(&surreal, notation_id, hop)
                                .await
                                .map(Json)
                                .map_err(terminal_from)
                        })
                        .name("resolve-review-notification-recipients")
                        .await?
                        .into_inner();
                    let record_client_handoff = matches!(hop, ReviewNotificationHop::Client)
                        && !plan.recipient_ids.is_empty();

                    for recipient_id in plan.recipient_ids.iter().copied() {
                        let email = Arc::clone(&self.email);
                        let surreal = self.surreal.clone();
                        let project_id = plan.project_id;
                        // The mailbox and the Project code are read inside the
                        // send step, never carried across it: this run's
                        // journaled value is `()`, so neither reaches the
                        // journal.
                        ctx.run(move || async move {
                            let target =
                                review_notification_target(&surreal, project_id, recipient_id)
                                    .await
                                    .map_err(terminal_from)?;
                            send_review_notification(
                                email,
                                notation_id,
                                project_id,
                                &target.project_code,
                                recipient_id,
                                &target.address,
                                hop,
                            )
                            .await
                            .map_err(terminal_from)
                        })
                        .name("send-review-notification")
                        .await?;
                    }

                    if record_client_handoff {
                        let surreal = self.surreal.clone();
                        ctx.run(move || async move {
                            record_notation_funnel_step(
                                &surreal,
                                notation_id,
                                WorkflowFunnelStep::Sent(telemetry::FunnelChannel::Email),
                            )
                            .await
                            .map_err(terminal_from)
                        })
                        .name("record-funnel-sent")
                        .await?;
                    }
                }
            }

            // The firm signing the closing letter (`firm_signature__*`)
            // closes the matter: flip the bound Project `open` → `closed`.
            // The symmetric bookend to the client-signed retainer that
            // opened it. Journaled so a replay reuses the outcome rather
            // than re-updating; skipped for ephemeral (notation-less)
            // workflows, which never carry a firm-signature step.
            if !body.ephemeral && workflows::closes_matter(&from_state) {
                let surreal = self.surreal.clone();
                ctx.run(move || async move {
                    workflows::close_matter(&surreal, notation_id)
                        .await
                        .map_err(|e| HandlerError::from(TerminalError::new(format!("close: {e}"))))
                })
                .name("close-matter")
                .await?;
            }

            Ok(Json(SignalResponse {
                next_state: next.as_str().to_string(),
            }))
        }
        .instrument(span)
        .await
    }

    #[restate_sdk::handler]
    async fn workflow_current_state(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<CurrentStateResponse>, HandlerError> {
        let span = traced_handler_span("workflow_current_state", ctx.headers(), ctx.key());
        async move {
            let state = ctx.get::<String>(WORKFLOW_STATE_KEY).await?;
            Ok(Json(CurrentStateResponse { state }))
        }
        .instrument(span)
        .await
    }
}

/// Pure state-machine step: given a spec, a current state, and a
/// condition, return the next state name. Surface as a
/// `TerminalError` (caller-visible failure, not Restate-retryable)
/// on `AlreadyTerminal`, `NoTransition`, or `UnknownState`, since
/// those are deterministic outcomes that a replay would reproduce.
fn next_state(
    spec: &WorkflowSpec,
    from: &StateName,
    condition: &str,
) -> Result<StateName, HandlerError> {
    advance(spec, from, condition).map_err(terminal_from)
}

/// The Restate adapter's mapping for shared-logic failures: both classes
/// surface as terminal handler errors — the handler's existing semantics,
/// which treat store failures as caller-visible rather than Restate-retryable.
/// The DBOS adapter (`dbos_notation.rs`) retries the Store class instead.
fn terminal_from(error: NotationLogicError) -> HandlerError {
    match error {
        NotationLogicError::Terminal(message) | NotationLogicError::Store(message) => {
            TerminalError::new(message).into()
        }
    }
}

async fn dispatch_workflow_step(
    deps: &StepDeps,
    notation_id: uuid::Uuid,
    next: &StateName,
    payload: Option<&str>,
) -> Result<Option<String>, HandlerError> {
    if !dispatches_side_effect(next) {
        return Ok(None);
    }
    dispatch_step(deps, notation_id, next, payload)
        .await
        .map_err(|e| HandlerError::from(TerminalError::new(format!("dispatch: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::{
        dispatch_workflow_step, next_state, questionnaire_back_target, review_copy,
        review_notification_hop, review_recipient_ids, send_review_notification,
        traced_handler_span, ReviewNotificationHop,
    };
    use std::sync::Arc;
    use workflows::{CapturingEmail, EmailPayload, StateName, StepDeps, WorkflowSpec};

    #[test]
    fn the_handler_root_span_is_kept_by_the_collector() {
        let headers = restate_sdk::prelude::HeaderMap::default();
        let fields = crate::span_capture::fields_of(|| {
            traced_handler_span("workflow_start", &headers, "notation-1")
        });
        assert_eq!(
            fields.get("navigator.keep").map(String::as_str),
            Some("true")
        );
    }

    const SPEC: &str = r"
BEGIN:
  _: client_name
client_name:
  _: client_email
client_email:
  _: END
END: {}
";

    #[test]
    fn questionnaire_back_retry_recovers_the_single_persisted_event() {
        let mut first_question = vec![("BEGIN".into(), "entity".into(), "_".into(), "e0".into())];
        let first_back = questionnaire_back_target(&StateName::from("entity"), &first_question)
            .expect("the first answered question backs up to BEGIN");
        assert_eq!(first_back, StateName::begin());
        first_question.push((
            "entity".into(),
            first_back.as_str().into(),
            "back".into(),
            "e0b".into(),
        ));
        assert_eq!(
            questionnaire_back_target(&StateName::from("entity"), &first_question),
            Some(StateName::begin()),
            "lost Restate result recovers the BEGIN target without another event"
        );

        let mut persisted = vec![
            (
                "BEGIN".into(),
                "client_name".into(),
                "_".into(),
                "e1".into(),
            ),
            (
                "client_name".into(),
                "client_email".into(),
                "_".into(),
                "e2".into(),
            ),
            (
                "client_email".into(),
                "client_phone".into(),
                "_".into(),
                "e3".into(),
            ),
        ];
        // First attempt appends successfully, then the process loses the
        // ctx.run result before updating the Restate object key.
        let first = questionnaire_back_target(&StateName::from("client_phone"), &persisted)
            .expect("back target");
        persisted.push((
            "client_phone".into(),
            first.as_str().into(),
            "back".into(),
            "e4".into(),
        ));
        // Retry still sees the old object key and must reuse e4, not append e5.
        let retry = questionnaire_back_target(&StateName::from("client_phone"), &persisted)
            .expect("recovered back target");
        assert_eq!(retry, first);
        assert_eq!(
            persisted.iter().filter(|(_, _, c, _)| c == "back").count(),
            1
        );
        // Once the key commit succeeds, a new intentional Back starts at
        // client_email and traverses one more earlier question.
        let earlier = questionnaire_back_target(&retry, &persisted).expect("second back");
        assert_eq!(earlier.as_str(), "client_name");
        persisted.push((
            retry.as_str().into(),
            earlier.as_str().into(),
            "back".into(),
            "e5".into(),
        ));
        assert_eq!(
            persisted.iter().filter(|(_, _, c, _)| c == "back").count(),
            2
        );
    }

    #[test]
    fn next_state_advances_through_a_questionnaire_walk() {
        let spec = WorkflowSpec::from_yaml(SPEC).unwrap();
        let n = next_state(&spec, &StateName::begin(), "_").unwrap();
        assert_eq!(n.as_str(), "client_name");
        let n = next_state(&spec, &n, "_").unwrap();
        assert_eq!(n.as_str(), "client_email");
        let n = next_state(&spec, &n, "_").unwrap();
        assert_eq!(n, StateName::end());
    }

    #[test]
    fn next_state_returns_terminal_error_on_already_end() {
        let spec = WorkflowSpec::from_yaml(SPEC).unwrap();
        let err = next_state(&spec, &StateName::end(), "_").unwrap_err();
        // HandlerError implements Debug only; format that for the
        // message assertion.
        assert!(format!("{err:?}").contains("terminal"));
    }

    #[test]
    fn next_state_returns_terminal_error_on_unknown_condition() {
        let spec = WorkflowSpec::from_yaml(SPEC).unwrap();
        let err = next_state(&spec, &StateName::begin(), "bogus").unwrap_err();
        assert!(format!("{err:?}").contains("no transition"));
    }

    #[test]
    fn workflow_transition_identifies_review_entry_without_premature_send() {
        assert!(matches!(
            super::workflow_funnel_step(
                &StateName::from("intake_persisted__client"),
                "rendered",
                &StateName::from("lawyer_review")
            ),
            Some(super::WorkflowFunnelStep::ReviewEntered)
        ));
        assert!(super::workflow_funnel_step(
            &StateName::from("lawyer_review"),
            "approved",
            &StateName::from("generate_pdf__draft")
        )
        .is_none());
        assert!(super::workflow_funnel_step(
            &StateName::from("generate_pdf__draft"),
            "pdf_persisted",
            &StateName::from("sent_for_signature__pending")
        )
        .is_none());
        assert!(super::workflow_funnel_step(
            &StateName::from("lawyer_review"),
            "changes_requested",
            &StateName::from("reask__client")
        )
        .is_none());
    }

    #[tokio::test]
    async fn notation_funnel_events_carry_ids_without_client_content() {
        use std::io::Write;
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);

        impl Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .expect("capture lock")
                    .extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buffer {
            type Writer = Buffer;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_ansi(false)
            .with_writer(Buffer(output.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let surreal = store::test_support::mem_surreal().await;
        let notation_id = store::test_support::seed_notation(&surreal).await;
        let project_id = store::notations::find_by_id(&surreal, notation_id)
            .await
            .expect("notation lookup")
            .expect("seeded notation")
            .project_id;

        super::record_notation_funnel_step(
            &surreal,
            notation_id,
            super::WorkflowFunnelStep::IntakeComplete,
        )
        .await
        .expect("intake funnel event");
        super::record_notation_funnel_step(
            &surreal,
            notation_id,
            super::WorkflowFunnelStep::ReviewEntered,
        )
        .await
        .expect("review funnel event");
        super::record_notation_funnel_step(
            &surreal,
            notation_id,
            super::WorkflowFunnelStep::Sent(telemetry::FunnelChannel::Email),
        )
        .await
        .expect("sent funnel event");

        let rendered = String::from_utf8(output.lock().expect("capture lock").clone())
            .expect("capture is UTF-8");
        let funnel_lines: Vec<_> = rendered
            .lines()
            .filter(|line| line.contains(r#""target":"funnel""#))
            .collect();
        assert_eq!(funnel_lines.len(), 3, "funnel: {rendered}");
        let funnel = funnel_lines.join("\n");
        assert!(
            funnel.contains("funnel.intake_complete"),
            "funnel: {funnel}"
        );
        assert!(funnel.contains("funnel.review_entered"), "funnel: {funnel}");
        assert!(funnel.contains("funnel.sent"), "funnel: {funnel}");
        assert!(funnel.contains(r#""channel":"email""#), "funnel: {funnel}");
        assert!(
            funnel.contains(&notation_id.to_string()),
            "funnel: {funnel}"
        );
        assert!(funnel.contains(&project_id.to_string()), "funnel: {funnel}");
        for forbidden in [
            "libra@example.com",
            "Libra",
            "libra-estate",
            "Estate Plan",
            "email=",
            "phone=",
            "name=",
            "address=",
            "project_code=",
        ] {
            assert!(!funnel.contains(forbidden), "{forbidden} leaked: {funnel}");
        }
    }

    async fn fs_storage(suite: &str) -> Arc<dyn cloud::StorageService> {
        Arc::new(
            cloud::FsStorage::new(
                std::env::temp_dir().join(format!("navigator-notation-service-{suite}")),
            )
            .await
            .expect("temp FsStorage"),
        )
    }

    #[tokio::test]
    async fn lawyer_review_dispatch_is_noop_but_sends_one_notification() {
        let email = Arc::new(CapturingEmail::new());
        let deps = StepDeps::new(email.clone(), fs_storage("lawyer-review").await);

        let payload = dispatch_workflow_step(
            &deps,
            uuid::Uuid::from_u128(1),
            &StateName::from("lawyer_review"),
            None,
        )
        .await
        .expect("human gate dispatch is a no-op");

        assert!(payload.is_none());
        assert!(email.captured().is_empty());

        send_review_notification(
            email.clone(),
            uuid::Uuid::from_u128(11),
            uuid::Uuid::from_u128(12),
            "sample-litigation",
            uuid::Uuid::from_u128(13),
            "lawyer@example.com",
            ReviewNotificationHop::Lawyer,
        )
        .await
        .expect("lawyer review notification");

        let captured = email.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].subject, "A draft is ready for your review");
        let html = captured[0]
            .html_body
            .as_deref()
            .expect("the firm layout is the html alternative");
        assert!(html.contains("/app/lawyer/notations/00000000-0000-0000-0000-00000000000b/review"));
        assert!(captured[0]
            .body
            .contains("/app/lawyer/notations/00000000-0000-0000-0000-00000000000b/review"));
        assert!(!captured[0].body.contains("Jane Roe"));
        assert!(!captured[0].body.contains("Cruller v. Prine"));
        assert!(!captured[0].body.contains("Estate Plan"));
    }

    #[tokio::test]
    async fn approved_review_sends_one_client_notification() {
        let email = Arc::new(CapturingEmail::new());
        send_review_notification(
            email.clone(),
            uuid::Uuid::from_u128(21),
            uuid::Uuid::from_u128(22),
            "sample-litigation",
            uuid::Uuid::from_u128(23),
            "client@example.com",
            ReviewNotificationHop::Client,
        )
        .await
        .expect("client review notification");

        let captured = email.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].subject, "Your reviewed draft is ready");
        let html = captured[0]
            .html_body
            .as_deref()
            .expect("the firm layout is the html alternative");
        assert!(html.contains("/app/projects/sample-litigation"));
        assert!(captured[0]
            .body
            .ends_with("/app/projects/sample-litigation"));
    }

    #[tokio::test]
    async fn no_lawyer_tier_participant_yields_no_recipient() {
        // The fixture designates a lawyer DRI, but `dri_person` is seeded at
        // the default `Role::Client` tier, so its participation is client-side
        // and the DRI marker resolves to nobody the firm may notify.
        let surreal = store::test_support::mem_surreal().await;
        let notation_id = store::test_support::seed_notation(&surreal).await;
        let plan = super::resolve_review_notification_plan(
            &surreal,
            notation_id,
            ReviewNotificationHop::Lawyer,
        )
        .await
        .expect("resolve review recipients");

        assert!(plan.recipient_ids.is_empty());
    }

    /// `ctx.run` persists this value in the Restate journal, which is an
    /// operator debugging surface. A mailbox names a client and a Project code
    /// names who retained the firm, so neither may be frozen into the plan —
    /// both are read inside the send step instead.
    #[tokio::test]
    async fn the_journaled_plan_carries_opaque_ids_only() {
        let surreal = store::test_support::mem_surreal().await;
        let notation_id = store::test_support::seed_notation(&surreal).await;
        let plan = super::resolve_review_notification_plan(
            &surreal,
            notation_id,
            ReviewNotificationHop::Client,
        )
        .await
        .expect("resolve review recipients");

        assert_eq!(
            plan.recipient_ids.len(),
            1,
            "the client DRI is the recipient"
        );
        let journaled = serde_json::to_string(&plan).expect("the plan is journaled as JSON");
        assert!(
            !journaled.contains('@'),
            "a mailbox reached the journal: {journaled}"
        );
        assert!(
            !journaled.contains("libra-estate"),
            "a Project code reached the journal: {journaled}"
        );
    }

    #[test]
    fn review_hops_cover_approval_and_exclude_reask_or_refusal() {
        let review = StateName::from("lawyer_review");
        assert!(matches!(
            review_notification_hop(&StateName::from("draft"), "ready", &review),
            Some(ReviewNotificationHop::Lawyer)
        ));
        assert!(matches!(
            review_notification_hop(&review, "approved", &StateName::from("generate_pdf__draft")),
            Some(ReviewNotificationHop::Client)
        ));
        assert!(review_notification_hop(
            &review,
            "changes_requested",
            &StateName::from("reask__client")
        )
        .is_none());
        assert!(review_notification_hop(&review, "rejected", &StateName::end()).is_none());
    }

    #[test]
    fn no_lawyer_participants_produce_no_recipient_ids() {
        assert!(review_recipient_ids(&[], ReviewNotificationHop::Lawyer).is_empty());
        let copy = review_copy(ReviewNotificationHop::Lawyer);
        assert_eq!(copy.subject, "A draft is ready for your review");
    }

    #[tokio::test]
    async fn dispatch_workflow_step_runs_dispatched_states() {
        let email = Arc::new(CapturingEmail::new());
        let deps = StepDeps::new(email.clone(), fs_storage("email-send").await);
        let email_payload =
            serde_json::to_string(&EmailPayload::new("Aries", "aries@example.com")).unwrap();

        let payload = dispatch_workflow_step(
            &deps,
            uuid::Uuid::from_u128(2),
            &StateName::from("email_send__welcome"),
            Some(&email_payload),
        )
        .await
        .expect("email_send dispatch succeeds");

        assert!(payload.is_none());
        let captured = email.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].to, "aries@example.com");
        assert_eq!(captured[0].template_slug.as_deref(), Some("welcome"));
    }
}
