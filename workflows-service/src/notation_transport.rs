//! Engine-neutral Notation ingress request and response bodies.

use serde::{Deserialize, Serialize};

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
/// ignore it. Restate callers propagate the start-time `ephemeral` flag on each
/// signal so runs without a Notation skip its journal. DBOS retains that flag
/// in keyed state.
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
