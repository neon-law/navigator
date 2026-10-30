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

pub const WORKFLOW: &str = "EmailSummary";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED";

pub type EmailSummaryWorkflow =
    WorkflowRef<EmailSummaryRequest, EmailSummaryReport, EmailSummaryError>;

/// Whether the `EmailSummary` twin is enabled in the DBOS host. Disabled
/// unless explicitly set, so enabling the executor alone cannot duplicate the
/// live Restate lane.
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED must be true or false"),
    }
}

/// The workflow id every delivery of one email receipt joins. The Restate
/// invocation key is the receipt id; preserving it as the DBOS id keeps
/// duplicate deliveries deduplicated on either engine.
#[must_use]
pub fn workflow_id(receipt_id: uuid::Uuid) -> String {
    format!("{WORKFLOW}:{receipt_id}")
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
    surreal: store::surreal::SurrealDb,
    storage: Arc<dyn StorageService>,
    slack: Arc<dyn SlackBot>,
    providers: Option<SummaryProviders>,
}

impl Context {
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        storage: Arc<dyn StorageService>,
        slack: Arc<dyn SlackBot>,
        providers: Option<SummaryProviders>,
    ) -> Self {
        Self {
            surreal,
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
        let surreal = context.surreal.clone();
        dbos::step("load-receipt", move || {
            let surreal = surreal.clone();
            async move {
                match store::email_receipts::find_by_id(&surreal, receipt_id).await {
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
        let surreal = context.surreal.clone();
        let receipt = receipt.clone();
        let matter_project_id = request.matter_project_id;
        dbos::step("check-sender", move || {
            let (storage, surreal, receipt) =
                (Arc::clone(&storage), surreal.clone(), receipt.clone());
            async move {
                check_sender_step(storage, &surreal, &receipt, matter_project_id)
                    .await
                    .map_err(|error| {
                        // Restate's `HandlerError` has no `Display`; journal
                        // its `Debug` so a recovery can see what failed.
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
            surreal: context.surreal.clone(),
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
    let surreal = context.surreal.clone();
    let config = request.gemini.clone();
    let project_id = request.project_id.clone();
    let matter_project_id = request.matter_project_id;
    dbos::step("summarize-gemini", move || {
        let (storage, surreal, receipt, config) = (
            Arc::clone(&storage),
            surreal.clone(),
            receipt.clone(),
            config.clone(),
        );
        let providers = providers.clone();
        let project_id = project_id.clone();
        async move {
            let started = std::time::Instant::now();
            let result = summarize_provider_authorized(
                storage,
                surreal,
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
                // Restate's `HandlerError` has no `Display`; journal its
                // `Debug` so a recovery can see what failed.
                dbos::Error::Application(EmailSummaryError::Provider(format!("{error:?}")))
            })
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::{enabled, workflow_id, ENABLED_ENV, WORKFLOW};

    #[test]
    fn registration_is_opt_in_and_rejects_invalid_values() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|key| (key == ENABLED_ENV).then(|| "false".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "true".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "yes".to_owned())).is_err());
    }

    #[test]
    fn every_delivery_of_one_receipt_joins_one_workflow_id() {
        let receipt_id = uuid::Uuid::now_v7();
        assert_eq!(
            workflow_id(receipt_id),
            format!("EmailSummary:{receipt_id}")
        );
        assert_eq!(workflow_id(receipt_id), workflow_id(receipt_id));
        assert_ne!(workflow_id(uuid::Uuid::now_v7()), workflow_id(receipt_id));
        assert_eq!(WORKFLOW, "EmailSummary");
    }
}
