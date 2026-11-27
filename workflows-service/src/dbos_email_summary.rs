//! DBOS adapter for one archived support-email summary.
//!
//! The webhook submits only an opaque receipt ID and immutable run
//! configuration, and the workflow id is derived from that receipt, so a
//! redelivered message joins the same durable run instead of summarizing
//! twice. Every archive read, provider request, and Slack delivery is a
//! named DBOS step mirroring the Restate `EmailSummary::run` journal, so a
//! recovery replays step results instead of repeating side effects.

use std::sync::Arc;

use cloud::StorageService;
use dbos::{WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use workflows::{EmailSummaryRequest, ProviderDelivery, SlackBot, SummaryProvider};

use crate::email_summary::{
    check_sender_step, summarize_provider_authorized, DeliveryTarget, EmailSummaryReport,
    SenderGate, SummaryProviders,
};

pub mod recovery;

pub const WORKFLOW: &str = "EmailSummary";

pub type EmailSummaryWorkflow =
    WorkflowRef<EmailSummaryRequest, EmailSummaryReport, EmailSummaryError>;

/// The workflow id every delivery of one email receipt joins. The Restate
/// invocation key is the receipt id; preserving it as the DBOS id keeps
/// duplicate deliveries deduplicated on either engine.
#[must_use]
pub fn workflow_id(receipt_id: uuid::Uuid) -> String {
    workflows::trigger::email_summary_workflow_id(receipt_id)
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum EmailSummaryError {
    #[error("the email receipt could not be loaded")]
    Receipt,
    #[error("summary run configuration does not match receipt")]
    Config,
    #[error("the sender gate step failed: {0}")]
    SenderGate(String),
    #[error("the summary provider step failed: {0}")]
    Provider(String),
    #[error("the summary delivery failed: {0}")]
    Delivery(String),
}

#[derive(Clone)]
pub struct Context {
    application_store: store::application_store::ApplicationStore,
    storage: Arc<dyn StorageService>,
    slack: Arc<dyn SlackBot>,
    providers: Option<SummaryProviders>,
}

impl Context {
    #[must_use]
    pub fn new(
        application_store: store::application_store::ApplicationStore,
        storage: Arc<dyn StorageService>,
        slack: Arc<dyn SlackBot>,
        providers: Option<SummaryProviders>,
    ) -> Self {
        Self {
            application_store,
            storage,
            slack,
            providers,
        }
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<EmailSummaryWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: EmailSummaryRequest| {
        let context = context.clone();
        async move { run_summary(context, request).await }
    })
}

/// The journaled sequence behind the Restate `run` handler. Step names match
/// the Restate journal so a run reads the same on either engine.
async fn run_summary(
    context: Context,
    request: EmailSummaryRequest,
) -> dbos::Result<EmailSummaryReport, EmailSummaryError> {
    let receipt_id = request.receipt_id;
    let receipt = {
        let application_store = context.application_store.clone();
        dbos::step("load-receipt", move || {
            let application_store = application_store.clone();
            async move {
                match store::email_receipts::find_by_id(&application_store, receipt_id).await {
                    Ok(Some(receipt)) => Ok(receipt),
                    Ok(None) | Err(_) => Err(dbos::Error::Application(EmailSummaryError::Receipt)),
                }
            }
        })
        .await?
    };

    if request.gemini.input_digest != receipt.raw_digest {
        return Err(dbos::Error::Application(EmailSummaryError::Config));
    }

    let gate = {
        let storage = Arc::clone(&context.storage);
        let application_store = context.application_store.clone();
        let receipt = receipt.clone();
        let matter_project_id = request.matter_project_id;
        dbos::step("check-sender", move || {
            let (storage, application_store, receipt) = (
                Arc::clone(&storage),
                application_store.clone(),
                receipt.clone(),
            );
            async move {
                check_sender_step(storage, &application_store, &receipt, matter_project_id)
                    .await
                    .map_err(|error| {
                        // Retain the shared step failure for recovery diagnostics.
                        dbos::Error::Application(EmailSummaryError::SenderGate(format!(
                            "{error:?}"
                        )))
                    })
            }
        })
        .await?
    };

    let gemini = match gate {
        SenderGate::Unknown { reason } => ProviderDelivery::Skipped { reason },
        SenderGate::Known => {
            summarize_step(&context, receipt_id, receipt.clone(), &request).await?
        }
    };

    let delivery_state = {
        let target = DeliveryTarget {
            application_store: context.application_store.clone(),
            slack: Arc::clone(&context.slack),
            storage: Arc::clone(&context.storage),
            receipt,
            config: request.gemini.clone(),
            channel_id: request.channel_id.clone(),
        };
        dbos::step("deliver-slack", move || {
            let target = target.clone();
            let gemini = gemini.clone();
            async move {
                target.deliver(gemini).await.map_err(|error| {
                    dbos::Error::Application(EmailSummaryError::Delivery(format!("{error:?}")))
                })
            }
        })
        .await?
    };

    Ok(EmailSummaryReport {
        receipt_id,
        delivery_state,
    })
}

/// The journaled provider step, run only for known senders. Mirrors the
/// Restate `EmailSummary` journal entry-for-entry.
async fn summarize_step(
    context: &Context,
    receipt_id: uuid::Uuid,
    receipt: store::email_receipts::EmailReceipt,
    request: &EmailSummaryRequest,
) -> dbos::Result<ProviderDelivery, EmailSummaryError> {
    let storage = Arc::clone(&context.storage);
    let providers = context.providers.clone();
    let application_store = context.application_store.clone();
    let config = request.gemini.clone();
    let project_id = request.project_id.clone();
    let matter_project_id = request.matter_project_id;
    dbos::step("summarize-gemini", move || {
        let (storage, application_store, receipt, config) = (
            Arc::clone(&storage),
            application_store.clone(),
            receipt.clone(),
            config.clone(),
        );
        let providers = providers.clone();
        let project_id = project_id.clone();
        async move {
            let started = std::time::Instant::now();
            let result = summarize_provider_authorized(
                storage,
                application_store,
                providers,
                SummaryProvider::Gemini,
                receipt.clone(),
                config,
                project_id,
                matter_project_id,
            )
            .await;
            telemetry::record_email_stage(
                &receipt_id.to_string(),
                telemetry::EmailStage::Summary,
                match &result {
                    Ok(ProviderDelivery::Succeeded { .. }) => telemetry::EmailOutcome::Succeeded,
                    Ok(ProviderDelivery::Skipped { .. }) => telemetry::EmailOutcome::Skipped,
                    _ => telemetry::EmailOutcome::Failed,
                },
                started.elapsed(),
            );
            result.map_err(|error| {
                // Retain the shared step failure for recovery diagnostics.
                dbos::Error::Application(EmailSummaryError::Provider(format!("{error:?}")))
            })
        }
    })
    .await
}

#[cfg(test)]
mod tests;
