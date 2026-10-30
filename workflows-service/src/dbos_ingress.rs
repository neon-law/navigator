//! Authenticated, internal handoff from the app to manually started DBOS work.
//!
//! Health remains anonymously readable. The POST handler requires a bearer
//! secret before decoding its bounded request body and checks the Project id
//! against the canonical code before admitting the workflow.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use store::surreal::SurrealDb;

use crate::dbos_email_summary::EmailSummaryWorkflow;
use crate::dbos_finance_aggregate::{self, FinanceAggregateWorkflow, RunRequest};
use crate::dbos_inbound_message_alert::InboundMessageAlertWorkflow;
use crate::dbos_nightly_project_update::NightlyWorkflow;
use crate::dbos_project_slack::{self, LaneKey, LaneRequest, ProjectSlackWorkflow};
use crate::dbos_reconcile::ReconcileWorkflow;
use crate::project_slack::{LaneError, MainPushRequest};

const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_WORKFLOW_ID_BYTES: usize = 160;

#[derive(Clone)]
pub struct FinanceIngress {
    token: Arc<str>,
    starter: Arc<dyn FinanceStarter>,
}

#[derive(Clone, Default)]
pub struct WorkerIngress {
    pub finance: Option<FinanceIngress>,
    pub reconcile: Option<ReconcileIngress>,
    pub nightly_project_update: Option<NightlyIngress>,
    pub email_summary: Option<EmailSummaryIngress>,
    pub inbound_message_alert: Option<AlertIngress>,
    pub project_slack: Option<ProjectSlackIngress>,
}

#[derive(Clone)]
pub struct ReconcileIngress {
    token: Arc<str>,
    workflow: ReconcileWorkflow,
}

#[derive(Clone)]
pub struct NightlyIngress {
    token: Arc<str>,
    workflow: NightlyWorkflow,
}

#[derive(Clone)]
pub struct EmailSummaryIngress {
    token: Arc<str>,
    workflow: EmailSummaryWorkflow,
}

#[derive(Clone)]
pub struct AlertIngress {
    token: Arc<str>,
    workflow: InboundMessageAlertWorkflow,
}

#[async_trait::async_trait]
trait FinanceStarter: Send + Sync {
    async fn start(&self, workflow_id: &str, request: RunRequest) -> Result<(), StartError>;
}

struct DbosFinanceStarter {
    workflow: FinanceAggregateWorkflow,
    dbos: dbos::DBOS,
    surreal: SurrealDb,
    postgres: store::postgres::PostgresDb,
}

impl ReconcileIngress {
    #[must_use]
    pub fn new(token: String, workflow: ReconcileWorkflow) -> Self {
        Self {
            token: Arc::from(token),
            workflow,
        }
    }
}

impl NightlyIngress {
    #[must_use]
    pub fn new(token: String, workflow: NightlyWorkflow) -> Self {
        Self {
            token: Arc::from(token),
            workflow,
        }
    }
}

impl EmailSummaryIngress {
    #[must_use]
    pub fn new(token: String, workflow: EmailSummaryWorkflow) -> Self {
        Self {
            token: Arc::from(token),
            workflow,
        }
    }
}

impl AlertIngress {
    #[must_use]
    pub fn new(token: String, workflow: InboundMessageAlertWorkflow) -> Self {
        Self {
            token: Arc::from(token),
            workflow,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartError {
    ProjectNotFound,
    Conflict,
    StartFailed,
}

#[async_trait::async_trait]
impl FinanceStarter for DbosFinanceStarter {
    async fn start(&self, workflow_id: &str, request: RunRequest) -> Result<(), StartError> {
        let project = store::projects::find_by_id(&self.surreal, request.project_id)
            .await
            .map_err(|_| StartError::StartFailed)?
            .ok_or(StartError::ProjectNotFound)?;
        if project.code != request.project_code {
            return Err(StartError::ProjectNotFound);
        }
        claim_request_identity::<
            crate::dbos_finance_aggregate::FinanceAggregateReport,
            crate::dbos_finance_aggregate::FinanceAggregateError,
            RunRequest,
        >(
            &self.postgres,
            &self.dbos,
            workflow_id,
            "FinanceAggregate",
            &request,
        )
        .await?;
        self.workflow
            .start_with(
                request,
                dbos::StartOptions {
                    workflow_id: Some(workflow_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| StartError::StartFailed)?;
        Ok(())
    }
}

async fn claim_request_identity<O, E, T>(
    postgres: &store::postgres::PostgresDb,
    dbos: &dbos::DBOS,
    workflow_id: &str,
    workflow_name: &str,
    request: &T,
) -> Result<(), StartError>
where
    T: serde::Serialize,
{
    use sha2::{Digest, Sha256};

    let encoded = serde_json::to_vec(request).map_err(|_| StartError::StartFailed)?;
    let fingerprint = Sha256::digest(encoded);
    let existing: Option<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT workflow_name, request_fingerprint FROM dbos_workflow_request WHERE workflow_id = $1",
    )
    .bind(workflow_id)
    .fetch_optional(postgres.pool())
    .await
    .map_err(|_| StartError::StartFailed)?;
    if let Some(existing) = existing {
        return if existing.0 == workflow_name && existing.1.as_slice() == fingerprint.as_slice() {
            Ok(())
        } else {
            Err(StartError::Conflict)
        };
    }
    let handle = dbos
        .retrieve_workflow::<O, E>(workflow_id)
        .map_err(|_| StartError::StartFailed)?;
    match handle.status().await {
        Ok(_) => {
            // A competing ingress may have committed its claim and started
            // DBOS after our first read. Re-read the claim before treating
            // this as an unclaimed legacy workflow.
            let claimed: Option<(String, Vec<u8>)> = sqlx::query_as(
                "SELECT workflow_name, request_fingerprint FROM dbos_workflow_request WHERE workflow_id = $1",
            )
            .bind(workflow_id)
            .fetch_optional(postgres.pool())
            .await
            .map_err(|_| StartError::StartFailed)?;
            return match claimed {
                Some(claimed)
                    if claimed.0 == workflow_name
                        && claimed.1.as_slice() == fingerprint.as_slice() =>
                {
                    Ok(())
                }
                // Mismatched claims and claimless legacy rows cannot be
                // rebound safely.
                Some(_) | None => Err(StartError::Conflict),
            };
        }
        Err(dbos::Error::WorkflowNotFound { .. }) => {}
        Err(_) => return Err(StartError::StartFailed),
    }
    sqlx::query(
        "INSERT INTO dbos_workflow_request (workflow_id, workflow_name, request_fingerprint) \
         VALUES ($1, $2, $3) ON CONFLICT (workflow_id) DO NOTHING",
    )
    .bind(workflow_id)
    .bind(workflow_name)
    .bind(fingerprint.as_slice())
    .execute(postgres.pool())
    .await
    .map_err(|_| StartError::StartFailed)?;
    let existing: (String, Vec<u8>) = sqlx::query_as(
        "SELECT workflow_name, request_fingerprint FROM dbos_workflow_request WHERE workflow_id = $1",
    )
    .bind(workflow_id)
    .fetch_one(postgres.pool())
    .await
    .map_err(|_| StartError::StartFailed)?;
    if existing.0 != workflow_name || existing.1.as_slice() != fingerprint.as_slice() {
        return Err(StartError::Conflict);
    }
    Ok(())
}

impl FinanceIngress {
    /// Validate the service credential before starting the DBOS worker.
    pub fn validate_token(token: &str) -> Result<(), &'static str> {
        if token.trim().is_empty() || token.len() > 4096 {
            return Err("NAVIGATOR_DBOS_INGRESS_TOKEN must be set when DBOS ingress is enabled");
        }
        Ok(())
    }

    /// Construct a dispatcher after [`Self::validate_token`] has succeeded.
    #[must_use]
    pub fn new(
        token: String,
        workflow: FinanceAggregateWorkflow,
        dbos: dbos::DBOS,
        surreal: SurrealDb,
        postgres: store::postgres::PostgresDb,
    ) -> Self {
        Self {
            token: Arc::from(token),
            starter: Arc::new(DbosFinanceStarter {
                workflow,
                dbos,
                surreal,
                postgres,
            }),
        }
    }

    #[cfg(test)]
    fn with_starter(token: &str, starter: Arc<dyn FinanceStarter>) -> Self {
        Self {
            token: Arc::from(token),
            starter,
        }
    }
}

#[derive(Debug, Deserialize)]
struct FinanceRequest {
    workflow_id: String,
    request: RunRequest,
}

#[derive(Debug, Serialize)]
struct Accepted {
    workflow_id: String,
}

pub fn add_routes(router: Router, ingress: WorkerIngress) -> Router {
    let router = if let Some(finance) = ingress.finance {
        router.route(
            "/internal/dbos/finance-aggregate",
            post(start_finance).with_state(finance),
        )
    } else {
        router
    };
    let router = if let Some(reconcile) = ingress.reconcile {
        router.route(
            "/internal/dbos/reconcile-invoices",
            post(start_reconcile).with_state(reconcile),
        )
    } else {
        router
    };
    let router = if let Some(nightly) = ingress.nightly_project_update {
        router.route(
            "/internal/dbos/nightly-project-update",
            post(start_nightly_project_update).with_state(nightly),
        )
    } else {
        router
    };
    let router = if let Some(email_summary) = ingress.email_summary {
        router.route(
            "/internal/dbos/email-summary",
            post(start_email_summary).with_state(email_summary),
        )
    } else {
        router
    };
    let router = if let Some(alert) = ingress.inbound_message_alert {
        router.route(
            "/internal/dbos/inbound-message-alert",
            post(start_inbound_message_alert).with_state(alert),
        )
    } else {
        router
    };
    let router = if let Some(project_slack) = ingress.project_slack {
        router.route(
            "/internal/dbos/project-slack",
            post(start_project_slack).with_state(project_slack),
        )
    } else {
        router
    };
    router.layer(axum::extract::DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn start_finance(
    State(ingress): State<FinanceIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: FinanceRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_request(&request)?;
    ingress
        .starter
        .start(&request.workflow_id, request.request)
        .await
        .map_err(|error| match error {
            StartError::ProjectNotFound => StatusCode::NOT_FOUND,
            StartError::Conflict => StatusCode::CONFLICT,
            StartError::StartFailed => StatusCode::SERVICE_UNAVAILABLE,
        })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

#[derive(Debug, Deserialize)]
struct ReconcileRequest {
    workflow_id: String,
}

async fn start_reconcile(
    State(ingress): State<ReconcileIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: ReconcileRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_reconcile_request(&request)?;
    ingress
        .workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&request.workflow_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

async fn start_nightly_project_update(
    State(ingress): State<NightlyIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: ReconcileRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_nightly_request(&request)?;
    ingress
        .workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&request.workflow_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

#[derive(Clone)]
pub struct ProjectSlackIngress {
    token: Arc<str>,
    starter: Arc<dyn ProjectSlackStarter>,
}

#[async_trait::async_trait]
trait ProjectSlackStarter: Send + Sync {
    /// `workflow_id` is validated and parsed by the route; `project_id`
    /// names the Project whose lane the request rides.
    async fn start(
        &self,
        workflow_id: &str,
        project_id: uuid::Uuid,
        request: LaneRequest,
    ) -> Result<(), StartError>;
}

struct DbosProjectSlackStarter {
    workflow: ProjectSlackWorkflow,
    dbos: dbos::DBOS,
    surreal: SurrealDb,
    postgres: store::postgres::PostgresDb,
}

#[async_trait::async_trait]
impl ProjectSlackStarter for DbosProjectSlackStarter {
    async fn start(
        &self,
        workflow_id: &str,
        project_id: uuid::Uuid,
        request: LaneRequest,
    ) -> Result<(), StartError> {
        store::projects::find_by_id(&self.surreal, project_id)
            .await
            .map_err(|_| StartError::StartFailed)?
            .ok_or(StartError::ProjectNotFound)?;
        claim_request_identity::<(), LaneError, LaneRequest>(
            &self.postgres,
            &self.dbos,
            workflow_id,
            dbos_project_slack::WORKFLOW,
            &request,
        )
        .await?;
        self.workflow
            .start_with(
                request,
                dbos::StartOptions {
                    workflow_id: Some(workflow_id),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| StartError::StartFailed)?;
        Ok(())
    }
}

impl ProjectSlackIngress {
    /// Construct a dispatcher after [`FinanceIngress::validate_token`] has
    /// succeeded — both doors share the one credential.
    #[must_use]
    pub fn new(
        token: String,
        workflow: ProjectSlackWorkflow,
        dbos: dbos::DBOS,
        surreal: SurrealDb,
        postgres: store::postgres::PostgresDb,
    ) -> Self {
        Self {
            token: Arc::from(token),
            starter: Arc::new(DbosProjectSlackStarter {
                workflow,
                dbos,
                surreal,
                postgres,
            }),
        }
    }

    #[cfg(test)]
    fn with_starter(token: &str, starter: Arc<dyn ProjectSlackStarter>) -> Self {
        Self {
            token: Arc::from(token),
            starter,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProjectSlackRequest {
    workflow_id: String,
    /// Present exactly when the id names the main-push lane.
    #[serde(default)]
    event: Option<MainPushRequest>,
}

/// Parse and cross-check a Project Slack request: the id must name one of
/// the two lanes, the event must ride its lane, and the event must be the
/// shape the portal door relays.
fn validate_project_slack_request(
    request: &ProjectSlackRequest,
) -> Result<(uuid::Uuid, LaneRequest), StatusCode> {
    if request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let Some((project_id, lane)) = dbos_project_slack::parse_workflow_id(&request.workflow_id)
    else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let lane_request = match (&lane, &request.event) {
        (LaneKey::ClientView, None) => LaneRequest::ClientView,
        (
            LaneKey::MainPush {
                short_sha,
                run_attempt,
            },
            Some(event),
        ) => {
            event.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
            if *short_sha != event.short_sha || *run_attempt != event.run_attempt {
                return Err(StatusCode::BAD_REQUEST);
            }
            LaneRequest::MainPush(event.clone())
        }
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    Ok((project_id, lane_request))
}

/// The Project Slack door: start (or join) the lane's workflow for one
/// event. The id is the idempotency key, so a relayed event that arrives
/// twice joins the first run instead of posting twice.
async fn start_project_slack(
    State(ingress): State<ProjectSlackIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: ProjectSlackRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let (project_id, lane_request) = validate_project_slack_request(&request)?;
    ingress
        .starter
        .start(&request.workflow_id, project_id, lane_request)
        .await
        .map_err(|error| match error {
            StartError::ProjectNotFound => StatusCode::NOT_FOUND,
            StartError::Conflict => StatusCode::CONFLICT,
            StartError::StartFailed => StatusCode::SERVICE_UNAVAILABLE,
        })?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

fn validate_reconcile_request(request: &ReconcileRequest) -> Result<(), StatusCode> {
    let Some(run_key) = request.workflow_id.strip_prefix("ReconcileInvoices:") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    if request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES
        || (!run_key.starts_with("manual-")
            && chrono::NaiveDate::parse_from_str(run_key, "%Y-%m-%d").is_err())
        || (run_key.starts_with("manual-")
            && uuid::Uuid::parse_str(run_key.trim_start_matches("manual-")).is_err())
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_nightly_request(request: &ReconcileRequest) -> Result<(), StatusCode> {
    let Some(run_key) = request.workflow_id.strip_prefix("NightlyProjectUpdate:") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    if request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES
        || chrono::NaiveDate::parse_from_str(run_key, "%Y-%m-%d").is_err()
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct EmailSummaryStart {
    workflow_id: String,
    request: workflows::EmailSummaryRequest,
}

#[derive(Debug, Deserialize)]
struct AlertStart {
    workflow_id: String,
    request: workflows::InboundMessageAlertRequest,
}

/// The event twins' ids are deterministic per Message/receipt — that
/// identity is the duplicate-delivery dedupe — so a caller may not split
/// the id from the payload it names.
fn validate_email_summary_request(request: &EmailSummaryStart) -> Result<(), StatusCode> {
    if request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES
        || request.workflow_id != crate::dbos_email_summary::workflow_id(request.request.receipt_id)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_alert_request(request: &AlertStart) -> Result<(), StatusCode> {
    if request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES
        || request.workflow_id
            != crate::dbos_inbound_message_alert::workflow_id(request.request.message_id)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

fn validate_request(request: &FinanceRequest) -> Result<(), StatusCode> {
    let Some(run_id) = request.workflow_id.strip_prefix("FinanceAggregate:") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    if run_id.is_empty()
        || request.workflow_id.len() > MAX_WORKFLOW_ID_BYTES
        || !run_id.starts_with("manual-")
        || uuid::Uuid::parse_str(run_id.trim_start_matches("manual-")).is_err()
        || !store::projects::is_valid_code(&request.request.project_code)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(())
}

async fn start_email_summary(
    State(ingress): State<EmailSummaryIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: EmailSummaryStart =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_email_summary_request(&request)?;
    ingress
        .workflow
        .start_with(
            request.request,
            dbos::StartOptions {
                workflow_id: Some(&request.workflow_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

async fn start_inbound_message_alert(
    State(ingress): State<AlertIngress>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Accepted>), StatusCode> {
    if !authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let request: AlertStart = serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    validate_alert_request(&request)?;
    ingress
        .workflow
        .start_with(
            request.request,
            dbos::StartOptions {
                workflow_id: Some(&request.workflow_id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            workflow_id: request.workflow_id,
        }),
    ))
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    constant_time_eq(value.as_bytes(), token.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    difference == 0
}

#[must_use]
pub fn workflow_id_for_run(run_id: &str) -> String {
    dbos_finance_aggregate::workflow_id(run_id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    use super::{
        add_routes, authorized, constant_time_eq, validate_alert_request,
        validate_email_summary_request, validate_nightly_request, validate_project_slack_request,
        validate_reconcile_request, validate_request, workflow_id_for_run, AlertStart,
        EmailSummaryStart, FinanceIngress, FinanceRequest, FinanceStarter, NightlyIngress,
        ProjectSlackIngress, ProjectSlackRequest, ProjectSlackStarter, ReconcileIngress,
        ReconcileRequest, StartError, WorkerIngress,
    };
    use crate::dbos_finance_aggregate::RunRequest;
    use crate::dbos_project_slack;
    use crate::dbos_project_slack::LaneRequest;
    use crate::project_slack::{CiOutcome, MainPushRequest};

    #[derive(Default)]
    struct Starts(AtomicUsize);

    #[async_trait::async_trait]
    impl FinanceStarter for Starts {
        async fn start(&self, _workflow_id: &str, _request: RunRequest) -> Result<(), StartError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn request(workflow_id: &str) -> FinanceRequest {
        FinanceRequest {
            workflow_id: workflow_id.into(),
            request: RunRequest {
                project_id: uuid::Uuid::nil(),
                project_code: "sample-firm".into(),
                parameters: None,
            },
        }
    }

    #[test]
    fn missing_and_wrong_bearer_values_are_denied() {
        assert!(!authorized(&HeaderMap::new(), "expected-secret"));
        let invalid = [(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer wrong-secret"),
        )]
        .into_iter()
        .collect();
        assert!(!authorized(&invalid, "expected-secret"));
        let valid = [(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer expected-secret"),
        )]
        .into_iter()
        .collect();
        assert!(authorized(&valid, "expected-secret"));
    }

    #[test]
    fn request_identity_must_name_only_a_bounded_finance_run_and_valid_project() {
        const RUN_ID: &str = "manual-0190e0d0-0000-7000-8000-000000000001";
        assert!(validate_request(&request(&format!("FinanceAggregate:{RUN_ID}"))).is_ok());
        assert_eq!(
            validate_request(&request(&format!("ReconcileInvoices:{RUN_ID}"))),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            validate_request(&request(&format!(
                "FinanceAggregate:manual-{}",
                "a".repeat(200)
            ))),
            Err(StatusCode::BAD_REQUEST)
        );
        let mut invalid_project = request(&format!("FinanceAggregate:{RUN_ID}"));
        invalid_project.request.project_code = "new".into();
        assert_eq!(
            validate_request(&invalid_project),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[test]
    fn nightly_project_update_ingress_accepts_only_a_namespaced_utc_date() {
        assert!(validate_nightly_request(&ReconcileRequest {
            workflow_id: "NightlyProjectUpdate:2026-10-22".into(),
        })
        .is_ok());
        for workflow_id in [
            "NightlyProjectUpdate:2026-02-30",
            "NightlyProjectUpdate:2026-10-22T05:55",
            "ReconcileInvoices:2026-10-22",
            "NightlyProjectUpdate:2026-10-22:extra",
        ] {
            assert_eq!(
                validate_nightly_request(&ReconcileRequest {
                    workflow_id: workflow_id.into(),
                }),
                Err(StatusCode::BAD_REQUEST),
                "unexpectedly accepted {workflow_id}"
            );
        }
    }

    #[test]
    fn reconcile_handoff_accepts_only_manual_or_daily_reconcile_ids() {
        assert!(validate_reconcile_request(&ReconcileRequest {
            workflow_id: "ReconcileInvoices:2026-10-22".into(),
        })
        .is_ok());
        assert!(validate_reconcile_request(&ReconcileRequest {
            workflow_id: "ReconcileInvoices:manual-0190e0d0-0000-7000-8000-000000000001".into(),
        })
        .is_ok());
        assert_eq!(
            validate_reconcile_request(&ReconcileRequest {
                workflow_id: "Archives:2026-10-22".into(),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            validate_reconcile_request(&ReconcileRequest {
                workflow_id: "ReconcileInvoices:manual-not-a-uuid".into(),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    fn email_summary_start(receipt_id: uuid::Uuid) -> EmailSummaryStart {
        let digest = "a".repeat(64);
        EmailSummaryStart {
            workflow_id: crate::dbos_email_summary::workflow_id(receipt_id),
            request: workflows::EmailSummaryRequest {
                receipt_id,
                matter_project_id: None,
                project_id: "synthetic-project".into(),
                channel_id: "C-SYNTHETIC".into(),
                gemini: workflows::EmailSummaryRunConfig::new(
                    workflows::SummaryProvider::Gemini,
                    "summary-model",
                    "global",
                    "summary-v1",
                    &digest,
                )
                .expect("valid test config"),
            },
        }
    }

    #[test]
    fn event_routes_bind_the_workflow_id_to_the_payload_it_names() {
        let mut email = email_summary_start(uuid::Uuid::now_v7());
        assert!(validate_email_summary_request(&email).is_ok());
        email.workflow_id = format!("EmailSummary:{}", uuid::Uuid::now_v7());
        assert_eq!(
            validate_email_summary_request(&email),
            Err(StatusCode::BAD_REQUEST)
        );
        email.workflow_id = "ReconcileInvoices:2026-10-22".into();
        assert_eq!(
            validate_email_summary_request(&email),
            Err(StatusCode::BAD_REQUEST)
        );

        let message_id = uuid::Uuid::now_v7();
        let alert = AlertStart {
            workflow_id: crate::dbos_inbound_message_alert::workflow_id(message_id),
            request: workflows::InboundMessageAlertRequest { message_id },
        };
        assert!(validate_alert_request(&alert).is_ok());
        let mismatched = AlertStart {
            workflow_id: crate::dbos_inbound_message_alert::workflow_id(uuid::Uuid::now_v7()),
            request: workflows::InboundMessageAlertRequest { message_id },
        };
        assert_eq!(
            validate_alert_request(&mismatched),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[tokio::test]
    async fn unauthorized_body_never_starts_a_workflow_and_valid_retries_keep_the_id() {
        let starts = Arc::new(Starts::default());
        let router = add_routes(
            axum::Router::new(),
            WorkerIngress {
                finance: Some(FinanceIngress::with_starter(
                    "expected-secret",
                    starts.clone(),
                )),
                ..WorkerIngress::default()
            },
        );
        let body = serde_json::to_vec(&json!({
            "workflow_id": "FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001",
            "request": {
                "project_id": uuid::Uuid::nil(),
                "project_code": "sample-firm"
            }
        }))
        .unwrap();
        let unauthorized = Request::builder()
            .method("POST")
            .uri("/internal/dbos/finance-aggregate")
            .body(Body::from("{malformed"))
            .unwrap();
        assert_eq!(
            router.clone().oneshot(unauthorized).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(starts.0.load(Ordering::SeqCst), 0);

        let wrong_workflow = Request::builder()
            .method("POST")
            .uri("/internal/dbos/finance-aggregate")
            .header("authorization", "Bearer expected-secret")
            .body(Body::from(
                String::from_utf8(body.clone())
                    .unwrap()
                    .replace("FinanceAggregate:", "ReconcileInvoices:"),
            ))
            .unwrap();
        assert_eq!(
            router
                .clone()
                .oneshot(wrong_workflow)
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(starts.0.load(Ordering::SeqCst), 0);

        for _ in 0..2 {
            let valid = Request::builder()
                .method("POST")
                .uri("/internal/dbos/finance-aggregate")
                .header("authorization", "Bearer expected-secret")
                .body(Body::from(body.clone()))
                .unwrap();
            assert_eq!(
                router.clone().oneshot(valid).await.unwrap().status(),
                StatusCode::ACCEPTED
            );
        }
        assert_eq!(starts.0.load(Ordering::SeqCst), 2);
        // DBOS's real-PG integration test proves these same stable IDs join
        // one durable workflow; the route forwards the exact ID each retry.
    }

    /// A starter that always fails the same way, to pin each status mapping.
    struct Refuses(StartError);

    #[async_trait::async_trait]
    impl FinanceStarter for Refuses {
        async fn start(&self, _workflow_id: &str, _request: RunRequest) -> Result<(), StartError> {
            Err(self.0)
        }
    }

    async fn post(
        router: &axum::Router,
        uri: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> StatusCode {
        let mut request = Request::builder().method("POST").uri(uri);
        if let Some(bearer) = bearer {
            request = request.header("authorization", format!("Bearer {bearer}"));
        }
        router
            .clone()
            .oneshot(request.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap()
            .status()
    }

    const FINANCE_BODY: &str = r#"{"workflow_id":"FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001","request":{"project_id":"00000000-0000-0000-0000-000000000000","project_code":"sample-firm"}}"#;

    #[test]
    fn token_validation_refuses_blank_and_oversized_secrets() {
        assert!(FinanceIngress::validate_token("worker-secret").is_ok());
        for token in [String::new(), "   ".to_owned(), "a".repeat(4097)] {
            assert_eq!(
                FinanceIngress::validate_token(&token),
                Err("NAVIGATOR_DBOS_INGRESS_TOKEN must be set when DBOS ingress is enabled")
            );
        }
    }

    #[test]
    fn bearer_comparison_needs_the_scheme_and_the_whole_secret() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secret-longer"));
        assert!(!constant_time_eq(b"", b"secret"));
        let basic = [(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Basic expected-secret"),
        )]
        .into_iter()
        .collect();
        assert!(!authorized(&basic, "expected-secret"));
        assert_eq!(
            workflow_id_for_run("manual-0190e0d0-0000-7000-8000-000000000001"),
            "FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001"
        );
    }

    #[test]
    fn finance_ids_must_be_manual_uuid_runs() {
        for workflow_id in [
            "FinanceAggregate:",
            "FinanceAggregate:2026-10-22",
            "FinanceAggregate:manual-not-a-uuid",
        ] {
            assert_eq!(
                validate_request(&request(workflow_id)),
                Err(StatusCode::BAD_REQUEST),
                "unexpectedly accepted {workflow_id}"
            );
        }
        assert_eq!(
            validate_reconcile_request(&ReconcileRequest {
                workflow_id: "ReconcileInvoices:2026-13-01".into(),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
        assert_eq!(
            validate_nightly_request(&ReconcileRequest {
                workflow_id: format!("NightlyProjectUpdate:{}", "9".repeat(200)),
            }),
            Err(StatusCode::BAD_REQUEST)
        );
    }

    #[tokio::test]
    async fn a_worker_without_enabled_workflows_exposes_no_start_route() {
        let router = add_routes(axum::Router::new(), WorkerIngress::default());
        for uri in [
            "/internal/dbos/finance-aggregate",
            "/internal/dbos/reconcile-invoices",
            "/internal/dbos/nightly-project-update",
            "/internal/dbos/project-slack",
        ] {
            assert_eq!(
                post(&router, uri, Some("anything"), "{}").await,
                StatusCode::NOT_FOUND
            );
        }
    }

    #[tokio::test]
    async fn finance_start_failures_map_to_distinct_statuses() {
        for (error, status) in [
            (StartError::ProjectNotFound, StatusCode::NOT_FOUND),
            (StartError::Conflict, StatusCode::CONFLICT),
            (StartError::StartFailed, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let router = add_routes(
                axum::Router::new(),
                WorkerIngress {
                    finance: Some(FinanceIngress::with_starter(
                        "expected-secret",
                        Arc::new(Refuses(error)),
                    )),
                    ..WorkerIngress::default()
                },
            );
            assert_eq!(
                post(
                    &router,
                    "/internal/dbos/finance-aggregate",
                    Some("expected-secret"),
                    FINANCE_BODY
                )
                .await,
                status
            );
            assert_eq!(
                post(
                    &router,
                    "/internal/dbos/finance-aggregate",
                    Some("expected-secret"),
                    "{not json"
                )
                .await,
                StatusCode::BAD_REQUEST
            );
        }
    }

    #[tokio::test]
    async fn an_oversized_body_is_refused_before_it_is_decoded() {
        let starts = Arc::new(Starts::default());
        let router = add_routes(
            axum::Router::new(),
            WorkerIngress {
                finance: Some(FinanceIngress::with_starter(
                    "expected-secret",
                    starts.clone(),
                )),
                ..WorkerIngress::default()
            },
        );
        let oversized = format!(r#"{{"workflow_id":"{}"}}"#, "a".repeat(17 * 1024));
        assert_eq!(
            post(
                &router,
                "/internal/dbos/finance-aggregate",
                Some("expected-secret"),
                &oversized
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(starts.0.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn reconcile_and_nightly_routes_authenticate_then_validate_then_start() {
        let router = add_routes(
            axum::Router::new(),
            WorkerIngress {
                finance: None,
                reconcile: Some(ReconcileIngress::new(
                    "worker-secret".into(),
                    crate::dbos_reconcile::unlaunched_workflow(),
                )),
                nightly_project_update: Some(NightlyIngress::new(
                    "worker-secret".into(),
                    crate::dbos_nightly_project_update::unlaunched_workflow().await,
                )),
                ..WorkerIngress::default()
            },
        );
        for (uri, valid, foreign) in [
            (
                "/internal/dbos/reconcile-invoices",
                r#"{"workflow_id":"ReconcileInvoices:2026-10-22"}"#,
                r#"{"workflow_id":"NightlyProjectUpdate:2026-10-22"}"#,
            ),
            (
                "/internal/dbos/nightly-project-update",
                r#"{"workflow_id":"NightlyProjectUpdate:2026-10-22"}"#,
                r#"{"workflow_id":"ReconcileInvoices:2026-10-22"}"#,
            ),
        ] {
            assert_eq!(
                post(&router, uri, None, valid).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                post(&router, uri, Some("wrong-secret"), valid).await,
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                post(&router, uri, Some("worker-secret"), "{not json").await,
                StatusCode::BAD_REQUEST
            );
            assert_eq!(
                post(&router, uri, Some("worker-secret"), foreign).await,
                StatusCode::BAD_REQUEST
            );
            // A well-formed, authenticated request reaches DBOS; an executor
            // that cannot start it answers "try again", never "accepted".
            assert_eq!(
                post(&router, uri, Some("worker-secret"), valid).await,
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
    }

    fn push(short_sha: &str, run_attempt: u32) -> MainPushRequest {
        MainPushRequest {
            short_sha: short_sha.to_string(),
            run_url: "https://github.com/an-organization/sample-push/actions/runs/1".to_string(),
            author_login: "octocat".to_string(),
            gate: CiOutcome::Pass,
            publish: Some(CiOutcome::Skipped),
            run_attempt,
        }
    }

    #[test]
    fn a_project_slack_request_must_name_one_lane_with_the_matching_event() {
        let project = uuid::Uuid::now_v7();
        let client_view = ProjectSlackRequest {
            workflow_id: dbos_project_slack::client_view_workflow_id(project),
            event: None,
        };
        assert_eq!(
            validate_project_slack_request(&client_view).expect("client view"),
            (project, LaneRequest::ClientView)
        );

        let id = dbos_project_slack::main_push_workflow_id(project, &push("abc1234", 1));
        let main_push = ProjectSlackRequest {
            workflow_id: id,
            event: Some(push("abc1234", 1)),
        };
        let (parsed_project, lane_request) =
            validate_project_slack_request(&main_push).expect("main push");
        assert_eq!(parsed_project, project);
        assert_eq!(lane_request, LaneRequest::MainPush(push("abc1234", 1)));

        // The event must ride its lane: a different sha or attempt in the
        // event than the id names would let one relayed event shadow another.
        let mismatch = ProjectSlackRequest {
            workflow_id: main_push.workflow_id.clone(),
            event: Some(push("def5678", 1)),
        };
        assert_eq!(
            validate_project_slack_request(&mismatch),
            Err(StatusCode::BAD_REQUEST)
        );
        let mismatched_attempt = ProjectSlackRequest {
            workflow_id: main_push.workflow_id.clone(),
            event: Some(push("abc1234", 2)),
        };
        assert_eq!(
            validate_project_slack_request(&mismatched_attempt),
            Err(StatusCode::BAD_REQUEST)
        );

        // A client-view lane carries no event; a main-push lane carries one.
        let view_with_event = ProjectSlackRequest {
            workflow_id: dbos_project_slack::client_view_workflow_id(project),
            event: Some(push("abc1234", 1)),
        };
        assert_eq!(
            validate_project_slack_request(&view_with_event),
            Err(StatusCode::BAD_REQUEST)
        );
        let viewless_push = ProjectSlackRequest {
            workflow_id: main_push.workflow_id.clone(),
            event: None,
        };
        assert_eq!(
            validate_project_slack_request(&viewless_push),
            Err(StatusCode::BAD_REQUEST)
        );

        // Not a Project Slack id at all, or malformed within it.
        for workflow_id in [
            "FinanceAggregate:manual-0190e0d0-0000-7000-8000-000000000001".to_string(),
            format!("ProjectSlack:{}:not-a-lane", project.simple()),
            format!("ProjectSlack:{}:abc1234:x", project.simple()),
        ] {
            let foreign = ProjectSlackRequest {
                workflow_id,
                event: None,
            };
            assert_eq!(
                validate_project_slack_request(&foreign),
                Err(StatusCode::BAD_REQUEST)
            );
        }
    }

    struct StartsProjectSlack(AtomicUsize);

    #[async_trait::async_trait]
    impl ProjectSlackStarter for StartsProjectSlack {
        async fn start(
            &self,
            _workflow_id: &str,
            _project_id: uuid::Uuid,
            _request: LaneRequest,
        ) -> Result<(), StartError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// A starter that always fails the same way, to pin each status mapping.
    struct RefusesProjectSlack(StartError);

    #[async_trait::async_trait]
    impl ProjectSlackStarter for RefusesProjectSlack {
        async fn start(
            &self,
            _workflow_id: &str,
            _project_id: uuid::Uuid,
            _request: LaneRequest,
        ) -> Result<(), StartError> {
            Err(self.0)
        }
    }

    const PROJECT_SLACK_BODY: &str = r#"{"workflow_id":"ProjectSlack:0190e0d0-0000-7000-8000-0000000000aa:abc1234:1","event":{"short_sha":"abc1234","run_url":"https://github.com/an-organization/sample-push/actions/runs/1","author_login":"octocat","gate":"pass","run_attempt":1}}"#;

    #[tokio::test]
    async fn the_project_slack_route_authenticates_then_validates_then_starts() {
        let starts = Arc::new(StartsProjectSlack(AtomicUsize::new(0)));
        let router = add_routes(
            axum::Router::new(),
            WorkerIngress {
                project_slack: Some(ProjectSlackIngress::with_starter(
                    "worker-secret",
                    starts.clone(),
                )),
                ..WorkerIngress::default()
            },
        );
        assert_eq!(
            post(
                &router,
                "/internal/dbos/project-slack",
                None,
                PROJECT_SLACK_BODY
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &router,
                "/internal/dbos/project-slack",
                Some("wrong-secret"),
                PROJECT_SLACK_BODY
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(
                &router,
                "/internal/dbos/project-slack",
                Some("worker-secret"),
                "{not json"
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(starts.0.load(Ordering::SeqCst), 0);

        // An event whose fields do not match its workflow id is refused.
        let mismatched = r#"{"workflow_id":"ProjectSlack:0190e0d0-0000-7000-8000-0000000000aa:abc1234:1","event":{"short_sha":"def5678","run_url":"https://github.com/an-organization/sample-push/actions/runs/1","author_login":"octocat","gate":"pass","run_attempt":1}}"#;
        assert_eq!(
            post(
                &router,
                "/internal/dbos/project-slack",
                Some("worker-secret"),
                mismatched
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(starts.0.load(Ordering::SeqCst), 0);

        for _ in 0..2 {
            assert_eq!(
                post(
                    &router,
                    "/internal/dbos/project-slack",
                    Some("worker-secret"),
                    PROJECT_SLACK_BODY
                )
                .await,
                StatusCode::ACCEPTED
            );
        }
        assert_eq!(starts.0.load(Ordering::SeqCst), 2);
        // The route forwards every accepted delivery; joining repeated ids
        // into one durable run is the claim layer's work, proven against
        // PostgreSQL in the integration suite.
    }

    #[tokio::test]
    async fn project_slack_start_failures_map_to_distinct_statuses() {
        for (error, status) in [
            (StartError::ProjectNotFound, StatusCode::NOT_FOUND),
            (StartError::Conflict, StatusCode::CONFLICT),
            (StartError::StartFailed, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let router = add_routes(
                axum::Router::new(),
                WorkerIngress {
                    project_slack: Some(ProjectSlackIngress::with_starter(
                        "expected-secret",
                        Arc::new(RefusesProjectSlack(error)),
                    )),
                    ..WorkerIngress::default()
                },
            );
            assert_eq!(
                post(
                    &router,
                    "/internal/dbos/project-slack",
                    Some("expected-secret"),
                    PROJECT_SLACK_BODY
                )
                .await,
                status
            );
        }
    }
}
