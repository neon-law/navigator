//! Inbox-process doors.
//!
//! The process backlog read and the body-carried process door behind
//! `inbox process`. The route itself is classified server-side at apply
//! time from the same transport facts the SendGrid webhook sees — a client
//! never posts a route. The message body never leaves the server: plans
//! carry classification metadata only.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::inbound_email::classify_inbound_facts;
use crate::session::SessionData;

fn map_inbox(error: store::inbox::InboxError) -> ApiError {
    match error {
        store::inbox::InboxError::NotFound => ApiError::NotFound,
        store::inbox::InboxError::Denied => ApiError::Forbidden,
        other => ApiError::Db(other.to_string()),
    }
}

/// The process view: classification metadata for one inbox message, plus
/// what a previous process run recorded. Every door serves this same shape,
/// so the CLI prints one plan from either a backlog listing, a status read,
/// or an apply.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessView {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub channel: String,
    pub provider_message_id: Option<String>,
    pub external_address: String,
    pub subject: Option<String>,
    /// The route a fresh classification assigns right now.
    pub computed_route: &'static str,
    pub computed_label: &'static str,
    /// The exact marker the classifier matched.
    pub reason: String,
    /// The route a previous process run recorded, if any.
    pub recorded_route: Option<String>,
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The matter the conversation is assigned to, for the plan's context.
    pub project_id: Option<Uuid>,
    pub occurred_at: String,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
}

fn view(row: store::inbox::UnprocessedRow, state: store::inbox::ProcessState) -> ProcessView {
    let decision = classify_inbound_facts(
        &row.external_address,
        row.subject.as_deref().unwrap_or_default(),
        &row.body,
    );
    ProcessView {
        id: row.id,
        conversation_id: row.conversation_id,
        channel: row.channel,
        provider_message_id: row.provider_message_id,
        external_address: row.external_address,
        subject: row.subject,
        computed_route: decision.route.as_str(),
        computed_label: decision.route.label(),
        reason: decision.reason,
        recorded_route: state.route,
        recorded_at: state.processed_at,
        project_id: row.project_id,
        occurred_at: row.occurred_at,
        inserted_at: row.inserted_at,
    }
}

/// The acting person's id. A session without one cannot act on the store.
fn actor_id(session: &SessionData) -> Result<Uuid, ApiError> {
    session.person_id.ok_or(ApiError::NotFound)
}

/// `GET /app/api/inbox/unprocessed` — the backlog, oldest first.
pub(crate) async fn unprocessed(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let rows = store::inbox::unprocessed_inbox(&state.surreal, actor_id)
        .await
        .map_err(map_inbox)?;
    let views = rows
        .into_iter()
        .map(|row| {
            view(
                row,
                store::inbox::ProcessState {
                    route: None,
                    processed_at: None,
                },
            )
        })
        .collect::<Vec<_>>();
    Ok(Json(views).into_response())
}

/// The body the process door reads: which message, and whether to record.
#[derive(Debug, Deserialize)]
pub(crate) struct ProcessRequest {
    /// The inbox message to plan or record.
    pub message: Uuid,
    /// False serves the plan only; true records the classification.
    #[serde(default)]
    pub apply: bool,
}

/// `POST /app/api/inbox/process` — the plan (apply: false) and the
/// recording (apply: true) behind `inbox process`, one constant-path door:
/// the parsed message id is body data, never URL material. The apply
/// classifies the message server-side from the same transport facts the
/// SendGrid webhook sees and records the route and processing time.
/// Idempotent: a re-run overwrites with the same truth and reports whether
/// the message was already recorded.
pub(crate) async fn process(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Json(request): Json<ProcessRequest>,
) -> Result<Response, ApiError> {
    let actor_id = actor_id(&session)?;
    let context = store::inbox::process_context(&state.surreal, actor_id, request.message)
        .await
        .map_err(map_inbox)?;
    if !request.apply {
        return Ok(Json(view(context.row, context.state)).into_response());
    }
    let decision = classify_inbound_facts(
        &context.row.external_address,
        context.row.subject.as_deref().unwrap_or_default(),
        &context.row.body,
    );
    let already_recorded = context.state.processed_at.is_some();
    let recorded_at = Utc::now();
    store::inbox::mark_processed(
        &state.surreal,
        request.message,
        decision.route.as_str(),
        recorded_at,
    )
    .await
    .map_err(map_inbox)?;
    let slack_notice = match context.row.project_id {
        Some(project_id) => notify_inbox_routed(&state, project_id).await,
        None => SlackNotice::NoProject,
    };
    Ok(Json(ProcessApplyView {
        result: view(
            context.row,
            store::inbox::ProcessState {
                route: Some(decision.route.as_str().to_owned()),
                processed_at: Some(recorded_at),
            },
        ),
        route: decision.route.as_str(),
        route_label: decision.route.label(),
        reason: decision.reason,
        recorded_at,
        already_recorded,
        slack_notice,
    })
    .into_response())
}

/// The apply response: the recorded verdict plus the view it landed on.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessApplyView {
    /// The message's plan, with the recording already applied.
    pub result: ProcessView,
    /// The storage spelling that was recorded.
    pub route: &'static str,
    pub route_label: &'static str,
    pub reason: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
    /// Whether a previous run had already recorded this message.
    pub already_recorded: bool,
    /// The best-effort Slack notice outcome.
    pub slack_notice: SlackNotice,
}

/// The best-effort Slack notice outcome. The recording succeeds whether or
/// not the notice lands; a notice failure never fails an apply. Like the
/// notify door, the notice is closed-vocabulary and mechanism-only —
/// `Navigator integration event: inbox_routed.` — never a subject, an
/// address, or a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SlackNotice {
    /// Posted to the matter's Firm-private channel.
    Notified,
    /// The message belongs to no matter, so there is no channel to tell.
    NoProject,
    /// The matter has no recorded Firm-private channel.
    NoChannel,
    /// The Firm's Slack credential was missing or refused the call.
    ProviderUnavailable,
}

/// Tell the matter's Firm-private channel that a message was routed. The
/// matter channel is never provisioned here — a missing channel is
/// reported so an operator runs `slack ensure` deliberately.
async fn notify_inbox_routed(state: &ApiState, project_id: Uuid) -> SlackNotice {
    let project = match store::projects::find_by_id(&state.surreal, project_id).await {
        Ok(Some(project)) => project,
        Ok(None) => return SlackNotice::NoProject,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: read the matter for the Slack notice");
            return SlackNotice::ProviderUnavailable;
        }
    };
    let slack = match state
        .integration_providers
        .slack(&state.surreal, project_id)
        .await
    {
        Ok(slack) => slack,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: resolve the Firm Slack provider");
            return SlackNotice::ProviderUnavailable;
        }
    };
    let channel = match slack.find_private_channel(&project.code).await {
        Ok(Some(channel)) => channel,
        Ok(None) => return SlackNotice::NoChannel,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: find the Firm-private channel");
            return SlackNotice::ProviderUnavailable;
        }
    };
    match workflows::post_slack_notice(
        slack.as_ref(),
        &channel.id,
        workflows::SlackNoticeEvent::InboxRouted,
    )
    .await
    {
        Ok(()) => SlackNotice::Notified,
        Err(error) => {
            tracing::warn!(error = %error, "inbox process: post the Slack notice");
            SlackNotice::ProviderUnavailable
        }
    }
}
