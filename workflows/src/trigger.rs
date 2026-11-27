//! Authenticated starts for DBOS workflows.

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

/// Start a DBOS schedule through authenticated ingress.
/// A caller retains its manual key when retrying the same request.
pub async fn start_dbos_schedule(
    ingress: &str,
    token: &str,
    service: &str,
    run_key: &str,
) -> Result<(), TriggerError> {
    if store::disabled_billing::disabled_billing_service(service) {
        return Err(TriggerError::Rejected {
            status: reqwest::StatusCode::GONE,
        });
    }
    start_dbos_at(
        ingress,
        token,
        "/internal/dbos/schedules",
        &serde_json::json!({
            "service": service, "run_key": run_key,
        }),
    )
    .await
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

/// Missing configuration for authenticated DBOS ingress.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ServiceTargetError {
    /// The selected DBOS ingress needs this variable set to a non-blank value.
    #[error("{0} must be set to start this workflow on DBOS")]
    Missing(&'static str),
}

/// Read the authenticated DBOS ingress configuration.
pub fn dbos_ingress(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<(String, String), ServiceTargetError> {
    parse_dbos_ingress(get(DBOS_INGRESS_URL_ENV), get(DBOS_INGRESS_TOKEN_ENV))
}

fn parse_dbos_ingress(
    ingress: Option<String>,
    token: Option<String>,
) -> Result<(String, String), ServiceTargetError> {
    let ingress = ingress
        .map(|url| url.trim().trim_end_matches('/').to_owned())
        .filter(|url| !url.is_empty())
        .ok_or(ServiceTargetError::Missing(DBOS_INGRESS_URL_ENV))?;
    let token = token
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .ok_or(ServiceTargetError::Missing(DBOS_INGRESS_TOKEN_ENV))?;
    Ok((ingress, token))
}

pub fn dbos_ingress_from_env() -> Result<(String, String), ServiceTargetError> {
    dbos_ingress(&|key| std::env::var(key).ok())
}

/// Mounted applications can omit workflow ingress for routes that only persist intake.
pub fn optional_dbos_ingress_from_env() -> Result<Option<(String, String)>, ServiceTargetError> {
    if std::env::var_os(DBOS_INGRESS_URL_ENV).is_none()
        && std::env::var_os(DBOS_INGRESS_TOKEN_ENV).is_none()
    {
        return Ok(None);
    }
    dbos_ingress_from_env().map(Some)
}

/// The base URL of the worker's authenticated DBOS ingress. Ungated: every
/// caller crate — `web`, the thin triggers, the worker's own handlers —
/// selects an ingress by these names without hosting the executor.
pub const DBOS_INGRESS_URL_ENV: &str = "NAVIGATOR_DBOS_INGRESS_URL";
/// The bearer secret for the worker's DBOS ingress.
pub const DBOS_INGRESS_TOKEN_ENV: &str = "NAVIGATOR_DBOS_INGRESS_TOKEN";
/// Queue one idempotent, authenticated EmailSummary recovery operation.
///
/// # Errors
/// Returns transport or ingress rejection.
pub async fn start_dbos_email_summary_recovery(
    ingress: &str,
    token: &str,
    request: &EmailSummaryRecoveryRequest,
) -> Result<(), TriggerError> {
    start_dbos_at(
        ingress,
        token,
        "/internal/dbos/email-summary/recover",
        request,
    )
    .await
}

/// One explicitly identified operator repair, retaining the archived receipt and request.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct EmailSummaryRecoveryRequest {
    pub operation_id: uuid::Uuid,
    pub from_recovery: Option<uuid::Uuid>,
    pub after_failed_result: Option<String>,
    pub request: crate::EmailSummaryRequest,
}

/// Global operation identity; reuse for another receipt is a request-claim conflict.
#[must_use]
pub fn email_summary_recovery_id(operation: uuid::Uuid) -> String {
    format!("EmailSummaryRecovery:{operation}")
}

/// A deterministic child identity makes a lost fork acknowledgement recoverable.
#[must_use]
pub fn email_summary_recovery_child(receipt: uuid::Uuid, operation: uuid::Uuid) -> String {
    format!("EmailSummary:{receipt}:recovery:{operation}")
}

/// Submit a scheduled run to authenticated DBOS ingress.
pub async fn run_dbos_cron(
    telemetry_name: &'static str,
    service: &'static str,
    route: &str,
    body: &serde_json::Value,
) -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init(telemetry_name);
    let (ingress, token) = dbos_ingress_from_env()?;
    let result = start_dbos_at(&ingress, &token, route, body).await;
    let outcome = match &result {
        Ok(()) => telemetry::outcome::ACCEPTED,
        Err(TriggerError::Transport) => telemetry::outcome::TRANSPORT_ERROR,
        Err(TriggerError::Rejected { .. }) => telemetry::outcome::REJECTED,
    };
    telemetry::record_trigger_fired(service, outcome);
    tracing::info!(service, outcome, "DBOS schedule submission");
    Ok(result?)
}

async fn start_dbos_at<B: Serialize + ?Sized>(
    ingress: &str,
    token: &str,
    route: &str,
    body: &B,
) -> Result<(), TriggerError> {
    if matches!(
        route,
        "/internal/dbos/reconcile-invoices" | "/internal/dbos/sandbox-credit-reset"
    ) {
        return Err(TriggerError::Rejected {
            status: reqwest::StatusCode::GONE,
        });
    }
    if token.trim().is_empty() {
        return Err(TriggerError::Transport);
    }
    let url = format!("{}{}", ingress.trim_end_matches('/'), route);
    let mut request = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .json(body)
        .timeout(std::time::Duration::from_secs(30));
    for (name, value) in telemetry::current_trace_context_headers() {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|_| TriggerError::Transport)?;
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

/// Opt-in for explicitly approved Inbox attachment filings.
pub const INBOX_ATTACHMENT_FILING_DBOS_ENABLED_ENV: &str =
    "NAVIGATOR_DBOS_INBOX_ATTACHMENT_FILING_ENABLED";

/// Read the shared Portal and worker attachment-filing switch.
#[must_use]
pub fn inbox_attachment_filing_enabled() -> bool {
    crate::flags::flag_enabled(
        INBOX_ATTACHMENT_FILING_DBOS_ENABLED_ENV,
        std::env::var(INBOX_ATTACHMENT_FILING_DBOS_ENABLED_ENV)
            .ok()
            .as_deref(),
    )
    .unwrap_or(false)
}

/// Queue a reviewed Inbox filing on the DBOS lane.
pub async fn start_approved_inbox_filing(
    approval: &store::inbox_triage::FilingApproval,
) -> Result<(), TriggerError> {
    let (ingress, token) = dbos_ingress_from_env().map_err(|_| TriggerError::Transport)?;
    start_dbos_at(
        &ingress,
        &token,
        "/internal/dbos/approved-inbox-filing",
        approval,
    )
    .await
}

/// Submit triage for a persisted inbound Message.
pub async fn start_inbound_triage(message_id: uuid::Uuid) -> Result<(), String> {
    let Some((ingress, token)) =
        optional_dbos_ingress_from_env().map_err(|error| error.to_string())?
    else {
        return Ok(());
    };
    start_dbos_at(
        &ingress,
        &token,
        "/internal/dbos/inbound-triage",
        &crate::InboundTriageRequest { message_id },
    )
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{start_dbos_finance_aggregate, start_dbos_reconcile, TriggerError};
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn disabled_billing_never_contacts_ingress() {
        let server = MockServer::start().await;
        for selection in store::disabled_billing::DISABLED_BILLING {
            let result =
                super::start_dbos_schedule(&server.uri(), "token", selection.name, "slot").await;
            assert!(matches!(
                result,
                Err(TriggerError::Rejected {
                    status: reqwest::StatusCode::GONE
                })
            ));
        }
        for result in [
            start_dbos_reconcile(&server.uri(), "token", &json!({})).await,
            super::start_dbos_sandbox_credit_reset(&server.uri(), "token", "2026-11").await,
        ] {
            assert!(matches!(
                result,
                Err(TriggerError::Rejected {
                    status: reqwest::StatusCode::GONE
                })
            ));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn dbos_handoff_reports_rejection_and_transport_failure() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/dbos/finance-aggregate"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let rejected = start_dbos_finance_aggregate(&server.uri(), "wrong", &json!({})).await;
        assert!(matches!(
            rejected,
            Err(TriggerError::Rejected { status }) if status.as_u16() == 401
        ));
        let unreachable =
            start_dbos_finance_aggregate("http://127.0.0.1:1", "worker-secret", &json!({})).await;
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
}

#[cfg(test)]
mod social_workflow_tests {
    use super::{social_publish_reconciler_workflow_id, social_publisher_workflow_id};
    use chrono::TimeZone;

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
    }
}
