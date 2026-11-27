//! DBOS adapter for the durable, metadata-only inbound-message Slack alert.
//!
//! The workflow id is derived from the persisted Message id, so repeated
//! provider deliveries of one message join the same durable run and the
//! existing `alert_state` receipts on the Message row decide whether a Slack
//! notice is posted at all. The context read, the alert delivery, and the
//! receipt read mirror the Restate `InboundMessageAlert::run` journal.

use dbos::{WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use workflows::{InboundMessageAlertReport, InboundMessageAlertRequest};

use crate::inbound_message_alert::{alert_context, AlertContextError, InboundMessageAlertService};

pub const WORKFLOW: &str = "InboundMessageAlert";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_INBOUND_MESSAGE_ALERT_ENABLED";

pub type InboundMessageAlertWorkflow =
    WorkflowRef<InboundMessageAlertRequest, InboundMessageAlertReport, InboundMessageAlertError>;

/// Whether the `InboundMessageAlert` twin is enabled in the DBOS host.
/// Disabled unless explicitly set, so enabling the executor alone cannot
/// duplicate the live Restate lane.
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_INBOUND_MESSAGE_ALERT_ENABLED must be true or false"),
    }
}

/// The workflow id every delivery of one inbound message joins. The Restate
/// invocation key is the message id; preserving it as the DBOS id keeps
/// duplicate deliveries deduplicated on either engine.
#[must_use]
pub fn workflow_id(message_id: uuid::Uuid) -> String {
    workflows::trigger::inbound_message_alert_workflow_id(message_id)
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum InboundMessageAlertError {
    #[error("inbound message not found")]
    MessageNotFound,
    #[error("inbound conversation not found")]
    ConversationNotFound,
    #[error("the message store read failed: {0}")]
    Store(String),
    #[error("the Slack alert step failed: {0}")]
    Deliver(String),
}

#[derive(Clone)]
pub struct Context {
    service: InboundMessageAlertService,
    postgres: store::postgres::PostgresDb,
}

impl Context {
    #[must_use]
    pub fn new(service: InboundMessageAlertService, postgres: store::postgres::PostgresDb) -> Self {
        Self { service, postgres }
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<InboundMessageAlertWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: InboundMessageAlertRequest| {
        let context = context.clone();
        // The SDK detaches workflow execution but awaits step bodies inline,
        // so the worker's own pool serves every Message read and receipt.
        async move {
            store::postgres::PostgresDb::with_application_db(
                context.postgres.clone(),
                run_alert(context, request),
            )
            .await
            .map_err(|_| {
                dbos::Error::Application(InboundMessageAlertError::Store(
                    "PostgreSQL application admission unavailable".to_owned(),
                ))
            })?
        }
    })
}

/// The journaled sequence behind the Restate `run` handler. Step names match
/// the Restate journal so a run reads the same on either engine.
async fn run_alert(
    context: Context,
    request: InboundMessageAlertRequest,
) -> dbos::Result<InboundMessageAlertReport, InboundMessageAlertError> {
    let message_id = request.message_id;
    let loaded = {
        let application_store = context.service.application_store().clone();
        dbos::step("load-inbound-message-context", move || {
            let application_store = application_store.clone();
            async move {
                alert_context(&application_store, message_id)
                    .await
                    .map_err(|error| match error {
                        AlertContextError::MessageNotFound => {
                            dbos::Error::Application(InboundMessageAlertError::MessageNotFound)
                        }
                        AlertContextError::ConversationNotFound => {
                            dbos::Error::Application(InboundMessageAlertError::ConversationNotFound)
                        }
                        AlertContextError::Store(inner) => dbos::Error::Application(
                            InboundMessageAlertError::Store(inner.to_string()),
                        ),
                    })
            }
        })
        .await?
    };
    let Some(loaded) = loaded else {
        return Ok(InboundMessageAlertReport {
            message_id,
            state: "skipped_non_inbound".into(),
        });
    };

    {
        let service = context.service.clone();
        let loaded = loaded.clone();
        dbos::step("post-inbound-message-alert", move || {
            let (service, loaded) = (service.clone(), loaded.clone());
            async move {
                service
                    .deliver_alert(message_id, &loaded)
                    .await
                    .map(|_| ())
                    .map_err(|error| {
                        // Retain the shared delivery failure for recovery diagnostics.
                        dbos::Error::Application(InboundMessageAlertError::Deliver(format!(
                            "{error:?}"
                        )))
                    })
            }
        })
        .await?;
    }

    let state = {
        let application_store = context.service.application_store().clone();
        dbos::step("read-inbound-message-alert-receipt", move || {
            let application_store = application_store.clone();
            async move {
                let receipt = store::inbox_alerts::receipt(&application_store, message_id)
                    .await
                    .map_err(|error| {
                        dbos::Error::Application(InboundMessageAlertError::Store(error.to_string()))
                    })?;
                dbos::Result::<_, InboundMessageAlertError>::Ok(
                    receipt.state.unwrap_or_else(|| "historical".into()),
                )
            }
        })
        .await?
    };

    Ok(InboundMessageAlertReport { message_id, state })
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
    fn every_delivery_of_one_message_joins_one_workflow_id() {
        let message_id = uuid::Uuid::now_v7();
        assert_eq!(
            workflow_id(message_id),
            format!("InboundMessageAlert:{message_id}")
        );
        assert_eq!(workflow_id(message_id), workflow_id(message_id));
        assert_ne!(workflow_id(uuid::Uuid::now_v7()), workflow_id(message_id));
        assert_eq!(WORKFLOW, "InboundMessageAlert");
    }
}
