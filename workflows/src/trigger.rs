//! Start a Restate workflow by POSTing to the ingress.
//!
//! The one shared way the application *kicks off* a durable workflow
//! from outside Restate. Two callers converge here:
//!
//! - the `archives` crate's `trigger` binary (the nightly CronJob),
//!   which fires the `Archives` export workflow once per night, and
//! - `web`'s Cron schedules controls (`portal::cron_schedules`), which queue
//!   any scheduled workflow on demand for testing / recovery.
//!
//! Both need the same wire shape — `POST {ingress}/{Service}/{key}/{handler}`
//! with an optional `Authorization: Bearer …` header. Restate Cloud
//! authenticates every ingress call with the tenant bearer token
//! (`RESTATE_AUTH_TOKEN`); the in-cluster Restate Operator used in
//! KIND does not. Passing `auth_token = None` (or an empty string)
//! sends no header at all, so the same code path works in both
//! environments — the exact contract the [`crate::RestateRuntime`]
//! adapter already follows for the `notation` service.
//!
//! `one_way = true` targets Restate's `/send` variant: the call
//! returns as soon as the invocation is *accepted* (Restate then runs
//! it to completion on the worker, owning the retry schedule). Use it
//! when the caller must not block on the whole run — e.g. an HTTP
//! handler that would otherwise hold a request open for the duration
//! of a 26-table snapshot.

use serde::Serialize;
use thiserror::Error;

/// Failure starting a workflow invocation through the ingress.
#[derive(Debug, Error)]
pub enum TriggerError {
    /// The HTTP request never produced a response (DNS, connect, timeout).
    #[error("workflow trigger transport failure")]
    Transport,
    /// The ingress responded with a non-2xx status. A `401` here is
    /// the classic "bearer token missing or wrong" — the bug that
    /// silently stopped the nightly archives email.
    #[error("workflow trigger rejected with status {}", status.as_u16())]
    Rejected { status: reqwest::StatusCode },
}

/// POST to the Restate ingress to start one invocation of
/// `{service}/{key}/{handler}`.
///
/// - `ingress` — the Restate ingress base URL (Restate Cloud in prod,
///   the in-cluster `restate` Service in KIND). A trailing slash is
///   trimmed.
/// - `auth_token` — `Some(non-empty)` attaches `Authorization: Bearer
///   …`; `None` or `Some("")` sends no header (KIND / dev).
/// - `service` / `key` / `handler` — the Restate virtual-object
///   coordinates, e.g. `("Archives", "2026-06-05", "run")`.
/// - `body` — JSON request body for the handler (`&serde_json::json!({})`
///   for handlers that take an empty struct).
/// - `one_way` — `true` appends `/send` so the call returns on
///   acceptance instead of blocking until the workflow completes.
///
/// On success returns the ingress response body (for `/send` this is
/// the JSON `{"invocationId": "inv_…"}` the caller can log).
///
/// # Errors
///
/// [`TriggerError::Transport`] when the request can't be sent;
/// [`TriggerError::Rejected`] on any non-success status.
#[tracing::instrument(
    level = "info",
    name = "workflow.trigger",
    skip(ingress, auth_token, key, body),
    fields(
        service = service,
        handler = handler,
        operation = "workflow trigger",
        one_way
    )
)]
pub async fn start_workflow<B: Serialize + ?Sized>(
    ingress: &str,
    auth_token: Option<&str>,
    service: &str,
    key: &str,
    handler: &str,
    body: &B,
    one_way: bool,
) -> Result<String, TriggerError> {
    let suffix = if one_way { "/send" } else { "" };
    let url = format!(
        "{}/{}/{}/{}{}",
        ingress.trim_end_matches('/'),
        service,
        key,
        handler,
        suffix
    );

    // Bound the POST so a hung or unreachable ingress can never leave a
    // trigger pod running indefinitely. A `CronJob` with
    // `concurrencyPolicy: Forbid` treats a still-running job as a reason to
    // skip the next schedule, so an unbounded request turns one transient
    // ingress stall into a permanently wedged schedule (this is one half of
    // how the nightly Archives trigger silently stopped firing). 30s is far
    // longer than a healthy one-way `/send` (milliseconds) yet short enough
    // that the Job's `activeDeadlineSeconds` backstop and the next schedule
    // both still apply.
    let mut req = reqwest::Client::new()
        .post(&url)
        .json(body)
        .timeout(std::time::Duration::from_secs(30));
    // Empty token is treated as absent: a mounted-but-empty secret
    // must not produce `Authorization: Bearer ` (Restate Cloud rejects
    // that as malformed). Mirrors `RestateRuntime::with_auth_token`.
    if let Some(token) = auth_token.filter(|t| !t.is_empty()) {
        req = req.bearer_auth(token);
    }

    // Inject the current span's W3C trace context (`traceparent`) so the
    // workflow handler can continue this trace across the Restate boundary
    // (extracted handler-side from `ctx.headers()`; see telemetry). Empty —
    // and a no-op — when OTLP is unconfigured or no sampled span is active, so
    // dev / KIND / OSS forks attach nothing. Only opaque trace context crosses
    // here, never a client field.
    for (name, value) in telemetry::current_trace_context_headers() {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(&value),
        ) {
            req = req.header(name, value);
        }
    }

    // Record the outcome as a metric (`navigator.workflow.trigger.fired`) and a
    // structured event on every path — safe fields and outcome counts only,
    // never identifiers or request content. This is the single instrumentation
    // point every trigger funnels through, so a service whose scheduled fire
    // silently stops shows up as a flat counter line and an absent "accepted" event.
    let Ok(resp) = req.send().await else {
        telemetry::record_trigger_fired(service, telemetry::outcome::TRANSPORT_ERROR);
        tracing::error!(
            service,
            handler,
            operation = "workflow trigger",
            outcome = telemetry::outcome::TRANSPORT_ERROR,
            "workflow trigger failed"
        );
        return Err(TriggerError::Transport);
    };
    let status = resp.status();
    let resp_body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        telemetry::record_trigger_fired(service, telemetry::outcome::REJECTED);
        tracing::error!(
            service,
            handler,
            operation = "workflow trigger",
            status = status.as_u16(),
            outcome = telemetry::outcome::REJECTED,
            "workflow trigger rejected by ingress"
        );
        return Err(TriggerError::Rejected { status });
    }
    telemetry::record_trigger_fired(service, telemetry::outcome::ACCEPTED);
    tracing::info!(
        service,
        handler,
        operation = "workflow trigger",
        status = status.as_u16(),
        outcome = telemetry::outcome::ACCEPTED,
        "workflow trigger accepted"
    );
    Ok(resp_body)
}

/// Authenticated DBOS handoff for a manually requested FinanceAggregate.
/// The caller must already have applied its user/Firm authorization check;
/// this bearer authenticates only the in-cluster app-to-worker hop.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "FinanceAggregate", operation = "DBOS ingress")
)]
pub async fn start_dbos_finance_aggregate<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/finance-aggregate", body).await
}

/// Authenticated handoff for the daily or manually requested reconciliation.
/// The daily ID is the same slot ID as the in-process DBOS schedule, so an
/// external Cron trigger joins that run instead of creating a second one.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "ReconcileInvoices", operation = "DBOS ingress")
)]
pub async fn start_dbos_reconcile<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/reconcile-invoices", body).await
}

/// Start a nightly Project sweep through the worker's authenticated DBOS
/// ingress. The workflow id is namespaced by its caller using its UTC date.
pub async fn start_dbos_nightly_project_update<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(
        ingress,
        token,
        "/internal/dbos/nightly-project-update",
        body,
    )
    .await
}

/// Start an `EmailSummary` run through the worker's authenticated DBOS
/// ingress, keyed by the receipt the run summarizes.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "EmailSummary", operation = "DBOS ingress")
)]
pub async fn start_dbos_email_summary<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/email-summary", body).await
}

/// Start an `InboundMessageAlert` run through the worker's authenticated
/// DBOS ingress, keyed by the inbound Message the alert reports.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "InboundMessageAlert", operation = "DBOS ingress")
)]
pub async fn start_dbos_inbound_message_alert<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/inbound-message-alert", body).await
}

/// Start (or join) a `ProjectSlack` lane through the worker's authenticated
/// DBOS ingress. The workflow id names the Project and the lane event.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "project-slack", operation = "DBOS ingress")
)]
pub async fn start_dbos_project_slack<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/project-slack", body).await
}

/// Start a `SocialPublisher` run through the worker's authenticated DBOS
/// ingress, keyed by the `(post, scheduled time)` pair the Restate ingress
/// used as the invocation key.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "SocialPublisher", operation = "DBOS ingress")
)]
pub async fn start_dbos_social_publisher<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/social-publisher", body).await
}

/// Start (or join) a `SocialPublishReconciler` slot through the worker's
/// authenticated DBOS ingress. A slot-floored id joins the run the worker's
/// internal schedule already started.
#[tracing::instrument(
    level = "info",
    skip(ingress, token, body),
    fields(service = "SocialPublishReconciler", operation = "DBOS ingress")
)]
pub async fn start_dbos_social_publish_reconciler<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(
        ingress,
        token,
        "/internal/dbos/social-publish-reconciler",
        body,
    )
    .await
}

// ---------------------------------------------------------------------------
// The ported services' workflow ids: one constructor per id format, shared by
// every caller (web, the triggers, the worker's own handlers) and by the
// worker's ingress validators. The id is the idempotency key — a caller and
// an ingress that disagree about its format either split one logical run in
// two or silently merge two — so the format is defined exactly once here.
// `workflows-service`'s twin modules delegate to these, and their ingress
// validators re-derive the same ids from the payload.

/// The `EmailSummary` workflow id for one archived receipt: every redelivery
/// of the message joins the same run.
#[must_use]
pub fn email_summary_workflow_id(receipt_id: uuid::Uuid) -> String {
    format!("EmailSummary:{receipt_id}")
}

/// The `InboundMessageAlert` workflow id for one inbound Message: every
/// provider redelivery joins the same run.
#[must_use]
pub fn inbound_message_alert_workflow_id(message_id: uuid::Uuid) -> String {
    format!("InboundMessageAlert:{message_id}")
}

/// The `InboundMessageAlert` recovery id for one attempt: a fresh id per
/// recovery tick, the DBOS counterpart of Restate's `{message_id}:{tick}`
/// invocation key. The plain per-message id deduplicates *deliveries*; a
/// recovery re-attempt of an alert that already completed (e.g. a Slack send
/// left `pending` by a rate limit) must NOT join that completed run — it
/// needs a new execution, one per recovery tick, so the two id formats
/// deliberately differ.
#[must_use]
pub fn inbound_message_alert_recovery_workflow_id(message_id: uuid::Uuid, tick: &str) -> String {
    format!("InboundMessageAlert:{message_id}:recover:{tick}")
}

/// The `ProjectSlack` client-view lane's workflow id for one Project: one
/// stable id per Project, so repeated views join the first run.
#[must_use]
pub fn project_slack_client_view_workflow_id(project_id: uuid::Uuid) -> String {
    format!("ProjectSlack:{}:client-view", project_id.simple())
}

/// The `ProjectSlack` main-push lane's workflow id for one
/// `(sha, run attempt)` per Project — the same idempotency key the Restate
/// object carried.
#[must_use]
pub fn project_slack_main_push_workflow_id(
    project_id: uuid::Uuid,
    short_sha: &str,
    run_attempt: u32,
) -> String {
    format!(
        "ProjectSlack:{}:{short_sha}:{run_attempt}",
        project_id.simple()
    )
}

/// The `SocialPublisher` workflow id for one `(post, scheduled time)` pair —
/// the Restate invocation key's twin.
#[must_use]
pub fn social_publisher_workflow_id(
    post_id: uuid::Uuid,
    publish_at: chrono::DateTime<chrono::Utc>,
) -> String {
    format!("SocialPublisher:{post_id}:{}", publish_at.timestamp())
}

/// The `SocialPublishReconciler` slot's workflow id — the five-minute UTC
/// slot, formatted to the minute.
#[must_use]
pub fn social_publish_reconciler_workflow_id(slot: chrono::DateTime<chrono::Utc>) -> String {
    format!("SocialPublishReconciler:{}", slot.format("%Y-%m-%dT%H-%M"))
}

/// Which ingress a caller of a ported service hands its start to, chosen by
/// that service's `NAVIGATOR_DBOS_*_ENABLED` switch.
///
/// Unlike [`TriggerTarget`] (the thin trigger binaries' selection, which
/// must fail a run when the Restate ingress is unconfigured), this caller
/// selection leaves the Restate branch's availability to the caller: a
/// portal without `RESTATE_BROKER_URL` reports "unavailable" today and keeps
/// doing so while the switch is off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceTarget {
    /// The worker's authenticated DBOS ingress; `ingress` has no trailing `/`.
    Dbos {
        /// The worker's DBOS ingress base URL.
        ingress: String,
        /// The bearer for the app-to-worker hop.
        token: String,
    },
    /// The Restate ingress while the service's switch is off — the caller's
    /// existing Restate-or-unavailable behavior.
    Restate,
}

/// A caller's ingress selection the environment does not complete.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ServiceTargetError {
    /// The service's switch is neither `true` nor `false`.
    #[error(transparent)]
    Switch(#[from] crate::flags::SettingsError),
    /// The selected DBOS ingress needs this variable set to a non-blank value.
    #[error("{0} must be set to start this workflow on DBOS")]
    Missing(&'static str),
}

/// Select a caller's ingress for one ported service from its DBOS switch.
///
/// With the switch on, both the worker's DBOS ingress URL and its bearer
/// must be non-blank — pointing a service at nothing is never the safe
/// default, so the caller reports the misconfiguration instead of silently
/// dropping the durable start. With the switch off (the default), the caller
/// keeps its existing Restate path.
///
/// # Errors
///
/// [`ServiceTargetError::Switch`] for a switch other than `true` / `false`;
/// [`ServiceTargetError::Missing`] for an absent or blank DBOS ingress
/// setting while the switch is on.
pub fn service_target(
    flag: &'static str,
    enabled: Option<&str>,
    dbos_ingress_url: Option<String>,
    dbos_ingress_token: Option<String>,
) -> Result<ServiceTarget, ServiceTargetError> {
    if crate::flags::flag_enabled(flag, enabled)? {
        let ingress = dbos_ingress_url
            .map(|url| url.trim().trim_end_matches('/').to_owned())
            .filter(|url| !url.is_empty())
            .ok_or(ServiceTargetError::Missing(DBOS_INGRESS_URL_ENV))?;
        let token = dbos_ingress_token
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty())
            .ok_or(ServiceTargetError::Missing(DBOS_INGRESS_TOKEN_ENV))?;
        Ok(ServiceTarget::Dbos { ingress, token })
    } else {
        Ok(ServiceTarget::Restate)
    }
}

/// The base URL of the worker's authenticated DBOS ingress. Ungated: every
/// caller crate — `web`, the thin triggers, the worker's own handlers —
/// selects an ingress by these names without hosting the executor.
pub const DBOS_INGRESS_URL_ENV: &str = "NAVIGATOR_DBOS_INGRESS_URL";
/// The bearer secret for the worker's DBOS ingress.
pub const DBOS_INGRESS_TOKEN_ENV: &str = "NAVIGATOR_DBOS_INGRESS_TOKEN";
#[cfg(feature = "dbos")]
/// The Restate ingress a trigger uses while its workflow's DBOS switch is off.
pub const RESTATE_INGRESS_URL_ENV: &str = "RESTATE_INGRESS_URL";
#[cfg(feature = "dbos")]
/// The optional Restate Cloud bearer.
pub const RESTATE_AUTH_TOKEN_ENV: &str = "RESTATE_AUTH_TOKEN";

/// The `EmailSummary` rollout switch. Default off; the Restate lane keeps
/// serving until cutover flips it.
pub const EMAIL_SUMMARY_ENABLED_ENV: &str = "NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED";
/// The `InboundMessageAlert` rollout switch. Default off.
pub const INBOUND_MESSAGE_ALERT_ENABLED_ENV: &str = "NAVIGATOR_DBOS_INBOUND_MESSAGE_ALERT_ENABLED";
/// The `project-slack` rollout switch. Default off.
pub const PROJECT_SLACK_ENABLED_ENV: &str = "NAVIGATOR_DBOS_PROJECT_SLACK_ENABLED";
/// The `SocialPublisher` rollout switch. Default off.
pub const SOCIAL_PUBLISHER_ENABLED_ENV: &str = "NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED";
/// The `SocialPublishReconciler` rollout switch. Default off.
pub const SOCIAL_PUBLISH_RECONCILER_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED";

#[cfg(feature = "dbos")]
/// Which durable ingress a thin daily trigger hands its run to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerTarget {
    /// The worker's authenticated DBOS ingress; `ingress` has no trailing `/`.
    Dbos { ingress: String, token: String },
    /// The Restate ingress during the transition.
    Restate {
        ingress: String,
        auth_token: Option<String>,
    },
}

#[cfg(feature = "dbos")]
/// A trigger's ingress settings the environment does not complete.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TriggerTargetError {
    /// The workflow-specific DBOS switch is neither `true` nor `false`.
    #[error(transparent)]
    Switch(#[from] crate::dbos_engine::SettingsError),
    /// The selected ingress needs this variable set to a non-blank value.
    #[error("{0} must be set for the selected workflow ingress")]
    Missing(&'static str),
}

#[cfg(feature = "dbos")]
/// Select a trigger's ingress from its workflow-specific DBOS switch.
///
/// `flag` follows the strict [`crate::dbos_engine::flag_enabled`] contract.
/// With the switch on, both the DBOS ingress URL and its bearer must be
/// non-blank; with it off, the Restate ingress URL is required.
///
/// # Errors
///
/// [`TriggerTargetError::Switch`] for a switch other than `true` / `false`;
/// [`TriggerTargetError::Missing`] for an absent or blank ingress setting.
pub fn trigger_target(
    flag: &'static str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<TriggerTarget, TriggerTargetError> {
    let required = |name: &'static str| {
        lookup(name)
            .filter(|value| !value.trim().is_empty())
            .ok_or(TriggerTargetError::Missing(name))
    };
    if crate::dbos_engine::flag_enabled(flag, lookup(flag).as_deref())? {
        let ingress = required(DBOS_INGRESS_URL_ENV)?
            .trim_end_matches('/')
            .to_owned();
        if ingress.trim().is_empty() {
            return Err(TriggerTargetError::Missing(DBOS_INGRESS_URL_ENV));
        }
        Ok(TriggerTarget::Dbos {
            ingress,
            token: required(DBOS_INGRESS_TOKEN_ENV)?,
        })
    } else {
        Ok(TriggerTarget::Restate {
            ingress: required(RESTATE_INGRESS_URL_ENV)?,
            auth_token: lookup(RESTATE_AUTH_TOKEN_ENV),
        })
    }
}

#[cfg(feature = "dbos")]
/// Whether the DBOS engine's standalone schedule owns a CronJob trigger's
/// cadence. The route-less engines (the digests, the nag, the notation reset,
/// the archives) expose no worker ingress route to submit to: when their
/// switch is on the engine fires them itself, and the trigger binary must
/// exit without submitting to Restate. The switch follows
/// [`crate::dbos_engine::flag_enabled`]'s strict contract.
///
/// # Errors
///
/// [`TriggerTargetError::Switch`] for a switch other than `true` / `false`.
pub fn dbos_owns_schedule(
    flag: &'static str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<bool, TriggerTargetError> {
    crate::dbos_engine::flag_enabled(flag, lookup(flag).as_deref())
        .map_err(TriggerTargetError::Switch)
}

async fn start_dbos_at<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    route: &str,
    body: &B,
) -> Result<(), TriggerError> {
    if token.trim().is_empty() {
        return Err(TriggerError::Transport);
    }
    let url = format!("{}{}", ingress.trim_end_matches('/'), route);
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| TriggerError::Transport)?;
    let status = response.status();
    if !status.is_success() {
        return Err(TriggerError::Rejected { status });
    }
    Ok(())
}

pub const SANDBOX_CREDIT_RESET_DBOS_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_SANDBOX_CREDIT_RESET_ENABLED";

pub async fn start_dbos_sandbox_credit_reset(
    ingress: &str,
    token: &str,
    period: &str,
) -> Result<(), TriggerError> {
    start_dbos_at(
        ingress,
        token,
        "/internal/dbos/sandbox-credit-reset",
        &serde_json::json!({ "period": period }),
    )
    .await
}

pub const INBOUND_TRIAGE_DBOS_ENABLED_ENV: &str = "NAVIGATOR_DBOS_INBOUND_TRIAGE_ENABLED";

/// Select one durable engine for a persisted inbound Message.
pub async fn start_inbound_triage(message_id: uuid::Uuid) -> Result<(), String> {
    let request = crate::InboundTriageRequest { message_id };
    let target = service_target(
        INBOUND_TRIAGE_DBOS_ENABLED_ENV,
        std::env::var(INBOUND_TRIAGE_DBOS_ENABLED_ENV)
            .ok()
            .as_deref(),
        std::env::var(DBOS_INGRESS_URL_ENV).ok(),
        std::env::var(DBOS_INGRESS_TOKEN_ENV).ok(),
    )
    .map_err(|error| error.to_string())?;
    match target {
        ServiceTarget::Dbos { ingress, token } => {
            start_dbos_at(&ingress, &token, "/internal/dbos/inbound-triage", &request).await
        }
        ServiceTarget::Restate => {
            let Some(ingress) = std::env::var("RESTATE_BROKER_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(());
            };
            start_workflow(
                &ingress,
                std::env::var("RESTATE_AUTH_TOKEN").ok().as_deref(),
                "InboundTriage",
                &message_id.to_string(),
                "run",
                &request,
                true,
            )
            .await
            .map(|_| ())
        }
    }
    .map_err(|error| error.to_string())
}

pub const YOUTUBE_COMMENTS_DBOS_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_YOUTUBE_COMMENTS_POLLER_ENABLED";

pub async fn start_dbos_youtube_comments<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    body: &B,
) -> Result<(), TriggerError> {
    start_dbos_at(ingress, token, "/internal/dbos/youtube-comments-poll", body).await
}

#[cfg(test)]
mod tests {
    use super::{
        start_dbos_finance_aggregate, start_dbos_nightly_project_update, start_dbos_reconcile,
        start_workflow, TriggerError,
    };
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;
    use wiremock::matchers::{body_partial_json, header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn dbos_handoff_reports_rejection_and_transport_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/reconcile-invoices"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let rejected = start_dbos_reconcile(&server.uri(), "wrong", &json!({})).await;
        assert!(matches!(
            rejected,
            Err(TriggerError::Rejected { status }) if status.as_u16() == 401
        ));
        let unreachable =
            start_dbos_nightly_project_update("http://127.0.0.1:1", "worker-secret", &json!({}))
                .await;
        assert!(matches!(unreachable, Err(TriggerError::Transport)));
    }

    #[tokio::test]
    async fn dbos_finance_handoff_requires_bearer_and_sends_the_canonical_run() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/finance-aggregate"))
            .and(header("authorization", "Bearer service-secret"))
            .and(body_partial_json(json!({
                "workflow_id": "FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001"
            })))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;

        start_dbos_finance_aggregate(
            &server.uri(),
            "service-secret",
            &json!({
                "workflow_id": "FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001",
                "request": { "project_id": "00000000-0000-0000-0000-000000000000", "project_code": "sample" }
            }),
        )
        .await
        .expect("authenticated service handoff");
        assert!(start_dbos_finance_aggregate(&server.uri(), "", &json!({}))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn dbos_sandbox_handoff_targets_its_own_monthly_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/sandbox-credit-reset"))
            .and(header("authorization", "Bearer worker-secret"))
            .and(body_partial_json(json!({"period": "2026-11"})))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        super::start_dbos_sandbox_credit_reset(&server.uri(), "worker-secret", "2026-11")
            .await
            .expect("sandbox rollover handoff");
    }

    #[tokio::test]
    async fn dbos_reconcile_handoff_targets_the_internal_allowlisted_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/reconcile-invoices"))
            .and(header("authorization", "Bearer worker-secret"))
            .and(body_partial_json(json!({
                "workflow_id": "ReconcileInvoices:2026-10-22"
            })))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        start_dbos_reconcile(
            &server.uri(),
            "worker-secret",
            &json!({ "workflow_id": "ReconcileInvoices:2026-10-22" }),
        )
        .await
        .expect("daily reconciliation accepted");
    }

    #[tokio::test]
    async fn dbos_nightly_handoff_targets_the_internal_allowlisted_route() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/nightly-project-update"))
            .and(header("authorization", "Bearer worker-secret"))
            .and(body_partial_json(json!({
                "workflow_id": "NightlyProjectUpdate:2026-10-22"
            })))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        start_dbos_nightly_project_update(
            &server.uri(),
            "worker-secret",
            &json!({ "workflow_id": "NightlyProjectUpdate:2026-10-22" }),
        )
        .await
        .expect("daily Project update accepted");
    }

    #[derive(Clone)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
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

    #[tokio::test]
    async fn posts_to_service_key_handler_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Archives/2026-06-05/run"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("{\"invocationId\":\"inv_1\"}"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let body = start_workflow(
            &server.uri(),
            None,
            "Archives",
            "2026-06-05",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap();
        assert!(body.contains("inv_1"));
    }

    #[tokio::test]
    async fn one_way_targets_the_send_variant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Archives/manual-7/run/send"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        start_workflow(
            &server.uri(),
            None,
            "Archives",
            "manual-7",
            "run",
            &json!({}),
            true,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn attaches_bearer_when_token_present() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .and(header("authorization", "Bearer s3cret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        start_workflow(
            &server.uri(),
            Some("s3cret"),
            "Archives",
            "d",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn sends_no_authorization_header_when_token_absent() {
        let server = MockServer::start().await;
        // Any request carrying an Authorization header must NOT match.
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        start_workflow(
            &server.uri(),
            None,
            "Archives",
            "d",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn empty_token_is_treated_as_absent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        start_workflow(
            &server.uri(),
            Some(""),
            "Archives",
            "d",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn passes_the_json_body_through() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Archives/d/run"))
            .and(body_partial_json(json!({"run_date": "2026-06-05"})))
            .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
            .expect(1)
            .mount(&server)
            .await;

        start_workflow(
            &server.uri(),
            None,
            "Archives",
            "d",
            "run",
            &json!({"run_date": "2026-06-05"}),
            false,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn rejected_error_and_trace_are_status_only() {
        let server = MockServer::start().await;
        let receipt = "receipt-sentinel-906";
        let invocation = "invocation-sentinel-906";
        let request_url = format!("{}/EmailSummary/{receipt}/run/send", server.uri());
        Mock::given(method("POST"))
            .and(path(format!("/EmailSummary/{receipt}/run/send")))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(format!("rejected {receipt} {invocation} {request_url}")),
            )
            .mount(&server)
            .await;

        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_ansi(false)
            .with_writer(Buffer(output.clone()))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let err = start_workflow(
            &server.uri(),
            None,
            "EmailSummary",
            receipt,
            "run",
            &json!({}),
            true,
        )
        .await
        .unwrap_err();
        drop(guard);
        let message = err.to_string();
        let trace = String::from_utf8(output.lock().expect("capture lock").clone())
            .expect("capture is UTF-8");

        match err {
            TriggerError::Rejected { status } => {
                assert_eq!(status.as_u16(), 401);
            }
            other @ TriggerError::Transport => panic!("expected Rejected, got {other:?}"),
        }
        assert_eq!(message, "workflow trigger rejected with status 401");
        for rendered in [&message, &trace] {
            assert!(!rendered.contains(receipt), "unsafe receipt in {rendered}");
            assert!(
                !rendered.contains(invocation),
                "unsafe invocation in {rendered}"
            );
            assert!(!rendered.contains(&request_url), "unsafe URL in {rendered}");
        }
        assert!(trace.contains("EmailSummary"));
        assert!(trace.contains("run"));
        assert!(trace.contains("rejected"));
        assert!(trace.contains("401"));
    }

    #[tokio::test]
    async fn transport_error_and_trace_are_status_only() {
        let ingress = "http://[::1";
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .without_time()
            .with_ansi(false)
            .with_writer(Buffer(output.clone()))
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let err = start_workflow(
            ingress,
            None,
            "Archives",
            "receipt-sentinel-transport-906",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap_err();
        drop(guard);
        let message = err.to_string();
        let trace = String::from_utf8(output.lock().expect("capture lock").clone())
            .expect("capture is UTF-8");

        assert!(matches!(err, TriggerError::Transport));
        assert_eq!(message, "workflow trigger transport failure");
        assert!(!message.contains(ingress));
        assert!(!trace.contains(ingress));
        assert!(!trace.contains("receipt-sentinel-transport-906"));
        assert!(trace.contains("Archives"));
        assert!(trace.contains("run"));
        assert!(trace.contains("transport_error"));
    }

    #[tokio::test]
    async fn unreachable_ingress_becomes_transport_error() {
        let err = start_workflow(
            "http://192.0.2.1:1",
            None,
            "Archives",
            "d",
            "run",
            &json!({}),
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, TriggerError::Transport));
    }
}

#[cfg(all(test, feature = "dbos"))]
mod target_tests {
    use super::{dbos_owns_schedule, trigger_target, TriggerTarget, TriggerTargetError};
    use crate::dbos_engine::SettingsError;

    const FLAG: &str = "NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED";

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn trigger_target_stays_on_restate_until_the_workflow_switch_is_true() {
        assert_eq!(
            trigger_target(FLAG, env(&[("RESTATE_INGRESS_URL", "http://restate:8080")])),
            Ok(TriggerTarget::Restate {
                ingress: "http://restate:8080".into(),
                auth_token: None,
            })
        );
        assert_eq!(
            trigger_target(
                FLAG,
                env(&[
                    (FLAG, "false"),
                    ("RESTATE_INGRESS_URL", "https://restate.example.invalid"),
                    ("RESTATE_AUTH_TOKEN", "tenant-bearer"),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "http://worker:9080"),
                ])
            ),
            Ok(TriggerTarget::Restate {
                ingress: "https://restate.example.invalid".into(),
                auth_token: Some("tenant-bearer".into()),
            })
        );
        assert_eq!(
            trigger_target(FLAG, env(&[])),
            Err(TriggerTargetError::Missing("RESTATE_INGRESS_URL"))
        );
    }

    #[test]
    fn trigger_target_on_dbos_requires_a_non_blank_ingress_and_bearer() {
        assert_eq!(
            trigger_target(
                FLAG,
                env(&[
                    (FLAG, " true "),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "http://worker:9080//"),
                    ("NAVIGATOR_DBOS_INGRESS_TOKEN", "worker-secret"),
                ])
            ),
            Ok(TriggerTarget::Dbos {
                ingress: "http://worker:9080".into(),
                token: "worker-secret".into(),
            })
        );
        for (pairs, missing) in [
            (
                &[(FLAG, "true"), ("NAVIGATOR_DBOS_INGRESS_TOKEN", "s")][..],
                "NAVIGATOR_DBOS_INGRESS_URL",
            ),
            (
                &[
                    (FLAG, "true"),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "  "),
                    ("NAVIGATOR_DBOS_INGRESS_TOKEN", "s"),
                ][..],
                "NAVIGATOR_DBOS_INGRESS_URL",
            ),
            (
                &[
                    (FLAG, "true"),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "///"),
                    ("NAVIGATOR_DBOS_INGRESS_TOKEN", "s"),
                ][..],
                "NAVIGATOR_DBOS_INGRESS_URL",
            ),
            (
                &[
                    (FLAG, "true"),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "http://worker"),
                ][..],
                "NAVIGATOR_DBOS_INGRESS_TOKEN",
            ),
            (
                &[
                    (FLAG, "true"),
                    ("NAVIGATOR_DBOS_INGRESS_URL", "http://worker"),
                    ("NAVIGATOR_DBOS_INGRESS_TOKEN", " "),
                ][..],
                "NAVIGATOR_DBOS_INGRESS_TOKEN",
            ),
        ] {
            assert_eq!(
                trigger_target(FLAG, env(pairs)),
                Err(TriggerTargetError::Missing(missing))
            );
        }
        assert_eq!(
            trigger_target(FLAG, env(&[(FLAG, "yes")])),
            Err(TriggerTargetError::Switch(SettingsError::Invalid(FLAG)))
        );
        assert_eq!(
            trigger_target(FLAG, env(&[("RESTATE_INGRESS_URL", " ")]))
                .unwrap_err()
                .to_string(),
            "RESTATE_INGRESS_URL must be set for the selected workflow ingress"
        );
    }

    #[test]
    fn dbos_owns_schedule_uses_the_strict_switch_contract() {
        let flag = "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED";
        assert_eq!(dbos_owns_schedule(flag, env(&[(flag, "true")])), Ok(true));
        assert_eq!(dbos_owns_schedule(flag, env(&[(flag, "false")])), Ok(false));
        // An absent or blank switch leaves the job on the Restate trigger.
        assert_eq!(dbos_owns_schedule(flag, env(&[])), Ok(false));
        assert_eq!(
            dbos_owns_schedule(flag, env(&[(flag, "yes")])),
            Err(TriggerTargetError::Switch(SettingsError::Invalid(flag)))
        );
    }
}

#[cfg(test)]
mod service_target_tests {
    use super::{
        service_target, social_publish_reconciler_workflow_id, social_publisher_workflow_id,
        ServiceTarget, ServiceTargetError,
    };
    use crate::flags::SettingsError;
    use chrono::TimeZone;

    const FLAG: &str = super::SOCIAL_PUBLISHER_ENABLED_ENV;

    fn target(
        enabled: Option<&str>,
        ingress: Option<&str>,
        token: Option<&str>,
    ) -> Result<ServiceTarget, ServiceTargetError> {
        service_target(
            FLAG,
            enabled,
            ingress.map(str::to_owned),
            token.map(str::to_owned),
        )
    }

    #[test]
    fn service_target_defaults_to_restate_until_the_switch_flips() {
        assert_eq!(target(None, None, None), Ok(ServiceTarget::Restate));
        assert_eq!(
            target(Some("false"), None, None),
            Ok(ServiceTarget::Restate)
        );
        assert_eq!(
            target(Some("yes"), Some("http://worker"), Some("t")),
            Err(ServiceTargetError::Switch(SettingsError::Invalid(FLAG)))
        );
    }

    #[test]
    fn service_target_on_dbos_requires_a_non_blank_ingress_and_bearer() {
        assert_eq!(
            target(
                Some("true"),
                Some(" http://worker:9083/ "),
                Some(" secret ")
            ),
            Ok(ServiceTarget::Dbos {
                ingress: "http://worker:9083".into(),
                token: "secret".into(),
            })
        );
        for (ingress, token, missing) in [
            (None, Some("t"), super::DBOS_INGRESS_URL_ENV),
            (Some("  "), Some("t"), super::DBOS_INGRESS_URL_ENV),
            (Some("///"), Some("t"), super::DBOS_INGRESS_URL_ENV),
            (Some("http://worker"), None, super::DBOS_INGRESS_TOKEN_ENV),
            (
                Some("http://worker"),
                Some(" "),
                super::DBOS_INGRESS_TOKEN_ENV,
            ),
        ] {
            assert_eq!(
                target(Some("true"), ingress, token),
                Err(ServiceTargetError::Missing(missing))
            );
        }
    }

    #[tokio::test]
    async fn dbos_social_handoffs_target_their_internal_allowlisted_routes() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/internal/dbos/social-publisher"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer worker-secret",
            ))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "workflow_id": "SocialPublisher:0190e0d0-0000-7000-8000-000000000001:1793577600",
                "request": { "post_id": "0190e0d0-0000-7000-8000-000000000001" }
            })))
            .respond_with(wiremock::ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        super::start_dbos_social_publisher(
            &server.uri(),
            "worker-secret",
            &serde_json::json!({
                "workflow_id": social_publisher_workflow_id(
                    uuid::Uuid::parse_str("0190e0d0-0000-7000-8000-000000000001").expect("uuid"),
                    chrono::Utc.timestamp_opt(1_793_577_600, 0).single().expect("ts"),
                ),
                "request": {
                    "post_id": "0190e0d0-0000-7000-8000-000000000001",
                    "publish_at": "2026-11-02T00:00:00Z"
                }
            }),
        )
        .await
        .expect("publisher handoff accepted");

        let slot = chrono::Utc
            .timestamp_opt(1_793_568_000, 0)
            .single()
            .expect("ts");
        assert_eq!(
            social_publish_reconciler_workflow_id(slot),
            "SocialPublishReconciler:2026-11-01T21-20"
        );
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(
                "/internal/dbos/social-publish-reconciler",
            ))
            .respond_with(wiremock::ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;
        super::start_dbos_social_publish_reconciler(
            &server.uri(),
            "worker-secret",
            &serde_json::json!({
                "workflow_id": social_publish_reconciler_workflow_id(slot),
                "request": { "tick": "2026-11-02T00:00:00+00:00" }
            }),
        )
        .await
        .expect("reconciler handoff accepted");
    }
}
