//! Explicit, receipt-owned EmailSummary recovery. No workflow rows are reset or deleted.

use dbos::sysdb::types::{StepRecord, WorkflowFilter, WorkflowRecord, WorkflowStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use store::email_receipts::summary_recovery as ownership;
use workflows::trigger::{
    email_summary_recovery_child, email_summary_recovery_id, EmailSummaryRecoveryRequest,
};
use workflows::ProviderDelivery;

use super::EmailSummaryError;
use crate::email_summary::EmailSummaryReport;

/// An internal operation selected by the same switch as the EmailSummary workflow.
pub const WORKFLOW: &str = "EmailSummaryRecovery";
pub type RecoveryWorkflow = dbos::WorkflowRef<RecoveryInput, RecoveryReport, RecoveryError>;

#[derive(Clone, Serialize, Deserialize)]
pub struct RecoveryInput {
    pub command: EmailSummaryRecoveryRequest,
    pub source_workflow_id: String,
    pub action: Action,
    pub target_workflow_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub source_workflow_id: String,
    pub target_workflow_id: String,
    pub action: Action,
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum RecoveryError {
    #[error("summary recovery input conflicts with its receipt or durable request")]
    Conflict,
    #[error("summary recovery requires compatible, recorded workflow stages")]
    Stage,
    #[error("summary recovery requires the source application version")]
    Version,
    #[error("another operation or an uncertain delivery requires reconciliation")]
    Admission,
    #[error("summary recovery storage is unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Resume,
    Fork(i32),
    RetryProvider(i32),
    ReconciledDelivery(i32),
}

impl Action {
    fn target(self, source: &str, command: &EmailSummaryRecoveryRequest) -> String {
        match self {
            Self::Resume => source.to_owned(),
            Self::Fork(_) | Self::RetryProvider(_) | Self::ReconciledDelivery(_) => {
                email_summary_recovery_child(command.request.receipt_id, command.operation_id)
            }
        }
    }
    fn delivery_rule(self, actor: Option<&str>) -> ownership::DeliveryRule<'_> {
        match self {
            Self::RetryProvider(_) => ownership::DeliveryRule::RetryFailureNotice(actor),
            Self::ReconciledDelivery(_) => ownership::DeliveryRule::Reconciled,
            Self::Fork(_) | Self::Resume => ownership::DeliveryRule::PreserveConfirmed,
        }
    }
}

fn verified_bound_target(input: &RecoveryInput) -> Result<(), RecoveryError> {
    if input.target_workflow_id
        != input
            .action
            .target(&input.source_workflow_id, &input.command)
        || input.command.from_recovery == Some(input.command.operation_id)
    {
        return Err(RecoveryError::Conflict);
    }
    Ok(())
}

fn target_can_handoff(status: WorkflowStatus) -> bool {
    status.is_terminal() || status == WorkflowStatus::MaxRecoveryAttemptsExceeded
}

fn verify_previous(
    previous: &WorkflowRecord,
    bound: &RecoveryInput,
    owner: uuid::Uuid,
    receipt: uuid::Uuid,
) -> Result<(), RecoveryError> {
    if previous.name.as_deref() != Some(WORKFLOW)
        || !previous.status.is_terminal()
        || bound.command.operation_id != owner
        || bound.command.request.receipt_id != receipt
    {
        return Err(RecoveryError::Admission);
    }
    verified_bound_target(bound)?;
    if let Some(output) = previous.output.as_deref() {
        let report: RecoveryReport =
            serde_json::from_str(output).map_err(|_| RecoveryError::Conflict)?;
        if report.source_workflow_id != bound.source_workflow_id
            || report.target_workflow_id != bound.target_workflow_id
            || report.action != bound.action
        {
            return Err(RecoveryError::Conflict);
        }
    }
    Ok(())
}

fn decode_recovery_input(record: &WorkflowRecord) -> Result<RecoveryInput, RecoveryError> {
    decode_input(record)
}

fn decode_input<T: serde::de::DeserializeOwned>(
    record: &WorkflowRecord,
) -> Result<T, RecoveryError> {
    let raw = record.input.as_deref().ok_or(RecoveryError::Conflict)?;
    match record.serialization.as_deref() {
        None | Some("rust_serde") => serde_json::from_str(raw).map_err(|_| RecoveryError::Conflict),
        Some("portable_json") => {
            let args: dbos::PortableWorkflowArgs =
                serde_json::from_str(raw).map_err(|_| RecoveryError::Conflict)?;
            if args.positional_args.len() != 1 || !args.named_args.is_empty() {
                return Err(RecoveryError::Conflict);
            }
            serde_json::from_value(args.positional_args[0].clone())
                .map_err(|_| RecoveryError::Conflict)
        }
        Some(_) => Err(RecoveryError::Conflict),
    }
}

fn validate_source(
    record: &WorkflowRecord,
    request: &workflows::EmailSummaryRequest,
    version: &str,
) -> Result<(), RecoveryError> {
    if record.name.as_deref() != Some(super::WORKFLOW) {
        return Err(RecoveryError::Conflict);
    }
    let base = super::workflow_id(request.receipt_id);
    let prefix = format!("{base}:recovery:");
    if record.workflow_id != base
        && !record
            .workflow_id
            .strip_prefix(&prefix)
            .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
    {
        return Err(RecoveryError::Conflict);
    }
    if record.app_version.as_deref() != Some(version) {
        return Err(RecoveryError::Version);
    }
    let stored: workflows::EmailSummaryRequest = decode_input(record)?;
    if serde_json::to_value(stored).map_err(|_| RecoveryError::Conflict)?
        != serde_json::to_value(request).map_err(|_| RecoveryError::Conflict)?
    {
        return Err(RecoveryError::Conflict);
    }
    Ok(())
}

fn action(record: &WorkflowRecord, steps: &[StepRecord]) -> Result<Action, RecoveryError> {
    match record.status {
        WorkflowStatus::Cancelled | WorkflowStatus::MaxRecoveryAttemptsExceeded => {
            Ok(Action::Resume)
        }
        WorkflowStatus::Error => {
            let failed = steps
                .iter()
                .find(|step| step.error.is_some())
                .ok_or(RecoveryError::Stage)?;
            if !matches!(
                failed.step_name.as_str(),
                "load-receipt" | "check-sender" | "summarize-gemini" | "deliver-slack"
            ) {
                return Err(RecoveryError::Stage);
            }
            Ok(Action::Fork(failed.step_id))
        }
        WorkflowStatus::Success => {
            let provider = steps
                .iter()
                .find(|step| step.step_name == "summarize-gemini")
                .ok_or(RecoveryError::Stage)?;
            let result: ProviderDelivery =
                serde_json::from_str(provider.output.as_deref().ok_or(RecoveryError::Stage)?)
                    .map_err(|_| RecoveryError::Stage)?;
            if matches!(result, ProviderDelivery::Failed { .. }) {
                return Ok(Action::RetryProvider(provider.step_id));
            }
            let delivery = steps
                .iter()
                .find(|step| step.step_name == "deliver-slack")
                .ok_or(RecoveryError::Stage)?;
            let state: String =
                serde_json::from_str(delivery.output.as_deref().ok_or(RecoveryError::Stage)?)
                    .map_err(|_| RecoveryError::Stage)?;
            if matches!(result, ProviderDelivery::Succeeded { .. })
                && matches!(state.as_str(), "unknown" | "sending")
            {
                return Ok(Action::ReconciledDelivery(delivery.step_id));
            }
            if matches!(result, ProviderDelivery::Succeeded { .. })
                && state == store::email_deliveries::FAILED
            {
                Ok(Action::Fork(delivery.step_id))
            } else {
                Err(RecoveryError::Stage)
            }
        }
        _ => Err(RecoveryError::Admission),
    }
}

async fn recorded(id: &str) -> dbos::Result<WorkflowRecord, RecoveryError> {
    dbos::list_workflows(&WorkflowFilter {
        workflow_ids: vec![id],
        load_input: true,
        load_output: true,
        limit: Some(1),
        ..Default::default()
    })
    .await?
    .pop()
    .ok_or_else(|| dbos::Error::Application(RecoveryError::Conflict))
}

/// Register the internal controller without capturing its own DBOS instance.
pub fn register(
    dbos: &dbos::DBOS,
    pg: store::postgres::PostgresDb,
    version: String,
) -> dbos::Result<RecoveryWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: RecoveryInput| {
        let pg = pg.clone();
        let version = version.clone();
        async move { run(pg, version, request).await }
    })
}

async fn run(
    pg: store::postgres::PostgresDb,
    version: String,
    input: RecoveryInput,
) -> dbos::Result<RecoveryReport, RecoveryError> {
    let receipt = input.command.request.receipt_id;
    let operation = input.command.operation_id;
    let source = recorded(&input.source_workflow_id).await?;
    validate_source(&source, &input.command.request, &version)?;
    let steps = dbos::list_workflow_steps(&input.source_workflow_id).await?;
    let action = action(&source, &steps)?;
    verified_bound_target(&input)?;
    if action != input.action {
        return Err(RecoveryError::Conflict.into());
    }
    let expected_owner = input.command.from_recovery;
    if let Some(owner) = expected_owner {
        let previous = recorded(&email_summary_recovery_id(owner)).await?;
        let previous_input = decode_recovery_input(&previous)?;
        verify_previous(&previous, &previous_input, owner, receipt)?;
        let candidate = &previous_input.target_workflow_id;
        let previous_child = dbos::list_workflows(&WorkflowFilter {
            workflow_ids: vec![candidate],
            limit: Some(1),
            ..Default::default()
        })
        .await?
        .pop();
        match previous_child {
            Some(child)
                if target_can_handoff(child.status)
                    && child.workflow_id == input.source_workflow_id =>
            {
                validate_source(&child, &input.command.request, &version)?;
                if child.workflow_id != previous_input.source_workflow_id
                    && child.forked_from.as_deref()
                        != Some(previous_input.source_workflow_id.as_str())
                {
                    return Err(RecoveryError::Conflict.into());
                }
            }
            None if previous.status != WorkflowStatus::Success
                && !matches!(previous_input.action, Action::Resume)
                && previous_input.source_workflow_id == input.source_workflow_id => {}
            _ => return Err(RecoveryError::Admission.into()),
        }
    }
    if !matches!(action, Action::Resume) {
        let child = email_summary_recovery_child(receipt, operation);
        let existing = dbos::list_workflows(&WorkflowFilter {
            workflow_ids: vec![&child],
            limit: Some(1),
            load_input: false,
            load_output: false,
            ..Default::default()
        })
        .await?;
        if !existing.is_empty() {
            let current = dbos::step("verify-existing-child-owner", || {
                let pg = pg.clone();
                async move {
                    ownership::current_operation(&pg, receipt)
                        .await
                        .map_err(|_| dbos::Error::Application(RecoveryError::Unavailable))
                }
            })
            .await?;
            if current != Some(operation) {
                return Err(RecoveryError::Conflict.into());
            }
        }
    }
    let actor = input.command.after_failed_result.clone();
    let digest = input.command.request.gemini.input_digest.clone();
    let fingerprint =
        crate::dbos_ingress::request_fingerprint(&input).map_err(|_| RecoveryError::Conflict)?;
    dbos::step("claim-receipt-and-delivery", || {
        let pg = pg.clone();
        let actor = actor.clone();
        let digest = digest.clone();
        let fingerprint = fingerprint.clone();
        async move {
            let claim: Option<(String,Vec<u8>)> = sqlx::query_as("SELECT workflow_name,request_fingerprint FROM dbos_workflow_request WHERE workflow_id=$1")
                .bind(email_summary_recovery_id(operation)).fetch_optional(pg.pool()).await.map_err(|_|RecoveryError::Unavailable)?;
            if !claim.is_some_and(|(name,stored)|name==WORKFLOW && stored==fingerprint) {
                return Err(RecoveryError::Conflict.into());
            }
            let result=ownership::claim(&pg,receipt,operation,expected_owner,&digest,action.delivery_rule(actor.as_deref())).await;
            result.map_err(|_| dbos::Error::Application(RecoveryError::Admission))
        }
    })
    .await?;
    let child_id = match action {
        Action::Resume => {
            dbos::resume_workflow::<EmailSummaryReport, EmailSummaryError>(
                &input.source_workflow_id,
            )
            .lift::<RecoveryError>()
            .await?;
            input.source_workflow_id.clone()
        }
        Action::Fork(step) | Action::RetryProvider(step) | Action::ReconciledDelivery(step) => {
            let child = email_summary_recovery_child(receipt, operation);
            let fork = dbos::Fork {
                original_id: &input.source_workflow_id,
                start_step: step,
                forked_id: Some(&child),
            };
            if dbos::fork_workflow_with::<EmailSummaryReport, EmailSummaryError>(
                fork,
                dbos::ForkOptions::default(),
            )
            .lift::<RecoveryError>()
            .await
            .is_err()
            {
                // A commit-before-acknowledgement failure may have created this exact child.
                let existing = recorded(&child).await?;
                validate_source(&existing, &input.command.request, &version)?;
                if existing.forked_from.as_deref() != Some(input.source_workflow_id.as_str()) {
                    return Err(RecoveryError::Conflict.into());
                }
            }
            child
        }
    };
    // The owner remains as the receipt's last operation. Another request can
    // hand it over only after verifying this controller and its actual child terminal.
    Ok(RecoveryReport {
        source_workflow_id: input.source_workflow_id,
        target_workflow_id: child_id,
        action,
    })
}

#[derive(Clone)]
pub struct RecoveryIngress {
    token: Arc<str>,
    dbos: dbos::DBOS,
    pg: store::postgres::PostgresDb,
    workflow: RecoveryWorkflow,
    version: String,
}

impl RecoveryIngress {
    #[must_use]
    pub fn new(
        token: String,
        dbos: dbos::DBOS,
        pg: store::postgres::PostgresDb,
        workflow: RecoveryWorkflow,
        version: String,
    ) -> Self {
        Self {
            token: token.into(),
            dbos,
            pg,
            workflow,
            version,
        }
    }
}

async fn read_record(
    dbos: &dbos::DBOS,
    id: &str,
) -> Result<Option<WorkflowRecord>, axum::http::StatusCode> {
    dbos.list_workflows(&WorkflowFilter {
        workflow_ids: vec![id],
        limit: Some(1),
        load_input: true,
        load_output: true,
        ..Default::default()
    })
    .await
    .map(|mut rows| rows.pop())
    .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)
}

fn conflict(_: RecoveryError) -> axum::http::StatusCode {
    axum::http::StatusCode::CONFLICT
}

async fn prepare(
    ingress: &RecoveryIngress,
    command: EmailSummaryRecoveryRequest,
) -> Result<RecoveryInput, axum::http::StatusCode> {
    use axum::http::StatusCode;
    let operation_id = email_summary_recovery_id(command.operation_id);
    if let Some(existing) = read_record(&ingress.dbos, &operation_id).await? {
        if existing.name.as_deref() != Some(WORKFLOW) {
            return Err(StatusCode::CONFLICT);
        }
        let previous = decode_recovery_input(&existing).map_err(conflict)?;
        if serde_json::to_value(&previous.command).map_err(|_| StatusCode::BAD_REQUEST)?
            != serde_json::to_value(&command).map_err(|_| StatusCode::BAD_REQUEST)?
        {
            return Err(StatusCode::CONFLICT);
        }
        return Ok(previous);
    }
    let receipt = command.request.receipt_id;
    if read_record(
        &ingress.dbos,
        &email_summary_recovery_child(receipt, command.operation_id),
    )
    .await?
    .is_some()
    {
        return Err(StatusCode::CONFLICT);
    }
    let owner = ownership::current_operation(&ingress.pg, receipt)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    let source_workflow_id = match owner {
        None => {
            if command.from_recovery.is_some() {
                return Err(StatusCode::CONFLICT);
            }
            super::workflow_id(receipt)
        }
        Some(owner) => {
            if command.from_recovery != Some(owner) {
                return Err(StatusCode::CONFLICT);
            }
            let previous = read_record(&ingress.dbos, &email_summary_recovery_id(owner))
                .await?
                .ok_or(StatusCode::CONFLICT)?;
            let previous_input = decode_recovery_input(&previous).map_err(conflict)?;
            verify_previous(&previous, &previous_input, owner, receipt).map_err(conflict)?;
            let candidate = &previous_input.target_workflow_id;
            match read_record(&ingress.dbos, candidate).await? {
                Some(child) => {
                    if !target_can_handoff(child.status) {
                        return Err(StatusCode::CONFLICT);
                    }
                    let expected_source = &previous_input.source_workflow_id;
                    if child.workflow_id != *expected_source
                        && child.forked_from.as_deref() != Some(expected_source.as_str())
                    {
                        return Err(StatusCode::CONFLICT);
                    }
                    child.workflow_id
                }
                None if previous.status != WorkflowStatus::Success
                    && !matches!(previous_input.action, Action::Resume) =>
                {
                    previous_input.source_workflow_id
                }
                None => return Err(StatusCode::CONFLICT),
            }
        }
    };
    let source = read_record(&ingress.dbos, &source_workflow_id)
        .await?
        .ok_or(StatusCode::NOT_FOUND)?;
    validate_source(&source, &command.request, &ingress.version).map_err(conflict)?;
    let steps = ingress
        .dbos
        .list_workflow_steps(&source_workflow_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let action = action(&source, &steps).map_err(conflict)?;
    ownership::preflight(
        &ingress.pg,
        receipt,
        &command.request.gemini.input_digest,
        action.delivery_rule(command.after_failed_result.as_deref()),
    )
    .await
    .map_err(|_| StatusCode::CONFLICT)?;
    let target_workflow_id = action.target(&source_workflow_id, &command);
    Ok(RecoveryInput {
        command,
        source_workflow_id,
        action,
        target_workflow_id,
    })
}

pub(crate) fn router(ingress: RecoveryIngress) -> axum::Router {
    axum::Router::new()
        .route(
            "/internal/dbos/email-summary/recover",
            axum::routing::post(start),
        )
        .with_state(ingress)
}

async fn start(
    axum::extract::State(ingress): axum::extract::State<RecoveryIngress>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<axum::http::StatusCode, axum::http::StatusCode> {
    use axum::http::StatusCode;
    if !crate::dbos_ingress::authorized(&headers, &ingress.token) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if body.len() > 16 * 1024 {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let command: EmailSummaryRecoveryRequest =
        serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
    let input = prepare(&ingress, command).await?;
    let id = email_summary_recovery_id(input.command.operation_id);
    crate::dbos_ingress::claim_summary_recovery_request(&ingress.pg, &ingress.dbos, &id, &input)
        .await?;
    if let Some(existing) = read_record(&ingress.dbos, &id).await? {
        if matches!(
            existing.status,
            WorkflowStatus::Cancelled | WorkflowStatus::MaxRecoveryAttemptsExceeded
        ) {
            ingress
                .dbos
                .resume::<RecoveryReport, RecoveryError>(&id)
                .await
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        }
    }
    ingress
        .workflow
        .start_with(
            input,
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests;
