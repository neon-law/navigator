//! Durable, metadata-only Slack notifications for persisted inbound Messages.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::inbox::{direction, Conversation, InboxError, Message};
use uuid::Uuid;
use workflows::SlackBot;

use crate::workflow_step_error::WorkflowStepError;

#[derive(Clone)]
pub struct InboundMessageAlertService {
    application_store: ApplicationStore,
    slack: Arc<dyn SlackBot>,
    messages_channel_id: String,
    staging_messages_channel_id: String,
    simulated: bool,
    slack_enabled: bool,
    public_origin: Option<String>,
}

impl InboundMessageAlertService {
    #[must_use]
    pub fn new(
        application_store: ApplicationStore,
        slack: Arc<dyn SlackBot>,
        messages_channel_id: String,
        staging_messages_channel_id: String,
        simulated: bool,
        slack_enabled: bool,
        public_origin: Option<String>,
    ) -> Self {
        Self {
            application_store,
            slack,
            messages_channel_id,
            staging_messages_channel_id,
            simulated,
            slack_enabled,
            public_origin,
        }
    }

    /// The shared `SurrealDB` handle the DBOS twin reads message context from.
    #[must_use]
    pub(crate) fn application_store(&self) -> &ApplicationStore {
        &self.application_store
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn journaled_context_without_the_new_firm_field_still_decodes() {
        let context: super::AlertContext = serde_json::from_value(serde_json::json!({
            "conversation_id": "00000000-0000-0000-0000-000000000001", "brand_key":"neon",
            "channel":"email", "interaction_type":"message",
        }))
        .unwrap();
        assert!(context.firm_id.is_nil());
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct AlertContext {
    conversation_id: Uuid,
    #[serde(default)]
    firm_id: Uuid,
    brand_key: String,
    channel: String,
    interaction_type: String,
}

/// Why the metadata-only context read failed. Terminal variants end the run
/// on either engine; `Store` is a retriable read failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AlertContextError {
    #[error("inbound message not found")]
    MessageNotFound,
    #[error("inbound conversation not found")]
    ConversationNotFound,
    #[error("inbound message store: {0}")]
    Store(#[from] InboxError),
}

/// The metadata-only context read shared by both engines. Message text,
/// external names, and addresses stay in the Firm inbox.
pub(crate) async fn alert_context(
    application_store: &ApplicationStore,
    message_id: Uuid,
) -> Result<Option<AlertContext>, AlertContextError> {
    let Some(message): Option<Message> =
        store::inbox::message_by_id(application_store, message_id).await?
    else {
        return Err(AlertContextError::MessageNotFound);
    };
    if message.direction != direction::INBOUND {
        return Ok(None);
    }
    let Some(conversation): Option<Conversation> =
        store::inbox::conversation_by_id(application_store, message.conversation_id).await?
    else {
        return Err(AlertContextError::ConversationNotFound);
    };
    Ok(Some(AlertContext {
        conversation_id: conversation.id,
        firm_id: conversation.firm_id,
        brand_key: conversation.brand_key.unwrap_or_else(|| "unbranded".into()),
        channel: conversation.channel,
        interaction_type: conversation.interaction_type,
    }))
}

impl InboundMessageAlertService {
    async fn reconcile_unknown_alert(
        &self,
        message_id: Uuid,
        channel_id: Option<&str>,
    ) -> Result<String, WorkflowStepError> {
        if let Some(channel) = channel_id {
            if let Ok(Some(found)) = self
                .slack
                .message_receipt_by_correlation(channel, &message_id.to_string())
                .await
            {
                if found.channel_id == channel && !found.timestamp.is_empty() {
                    store::inbox_alerts::record(
                        &self.application_store,
                        message_id,
                        "notified",
                        Some(&found.timestamp),
                    )
                    .await
                    .map_err(WorkflowStepError::retryable)?;
                    return Ok("notified".into());
                }
            }
        }
        store::inbox_alerts::record(&self.application_store, message_id, "unknown", None)
            .await
            .map_err(WorkflowStepError::retryable)?;
        Ok("unknown".into())
    }

    /// Deliver one alert. Shared by the Restate handler and the DBOS twin
    /// (`dbos_inbound_message_alert`) so Slack routing stays single-sourced.
    pub(crate) async fn deliver_alert(
        &self,
        message_id: Uuid,
        _context: &AlertContext,
    ) -> Result<String, WorkflowStepError> {
        use workflows::post_slack_notification::{classify_post_notice_attempt, PostNoticeAttempt};

        let Some(context) = alert_context(&self.application_store, message_id)
            .await
            .map_err(WorkflowStepError::retryable)?
        else {
            return Ok("skipped_non_inbound".into());
        };
        let receipt = store::inbox_alerts::receipt(&self.application_store, message_id)
            .await
            .map_err(WorkflowStepError::retryable)?;
        if receipt.state.as_deref() == Some("unknown") {
            return self
                .reconcile_unknown_alert(message_id, receipt.channel_id.as_deref())
                .await;
        }
        if receipt.state.as_deref() != Some("pending") {
            return Ok(receipt.state.unwrap_or_else(|| "historical".into()));
        }
        // A deployment-wide channel belongs to the deployment's Slack Firm.
        // Never route another Firm's inbound metadata into it. Invalid
        // explicit configuration fails closed.
        let owning_firm = match workflows::firm_slack::DeploymentSlackFirm::from_lookup(|key| {
            std::env::var(key).ok()
        }) {
            Ok(firm) => firm
                .resolve(&self.application_store)
                .await
                .map_err(WorkflowStepError::retryable)?,
            Err(_) => None,
        };
        let channel_id = owning_firm.and_then(|firm_id| {
            workflows::post_slack_notification::select_post_notice_channel(
                &workflows::post_slack_notification::PostNoticeChannels {
                    firm_id,
                    production: Some(&self.messages_channel_id),
                    simulated: Some(&self.staging_messages_channel_id),
                },
                context.firm_id,
                self.simulated,
            )
        });
        let origin = self
            .public_origin
            .as_deref()
            .and_then(|origin| reqwest::Url::parse(origin).ok())
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
            });
        let (Some(channel_id), Some(mut inbox_url), true) =
            (channel_id, origin, self.slack_enabled)
        else {
            store::inbox_alerts::record(&self.application_store, message_id, "disabled", None)
                .await
                .map_err(WorkflowStepError::retryable)?;
            return Ok("disabled".into());
        };
        inbox_url.set_path(&format!("/app/inbox/{}", context.conversation_id));
        inbox_url.set_query(None);
        inbox_url.set_fragment(None);
        if !store::inbox_alerts::reserve(&self.application_store, message_id, channel_id)
            .await
            .map_err(WorkflowStepError::retryable)?
        {
            return Ok("unknown".into());
        }
        let text = workflows::render_inbound_message_alert(
            &context.brand_key,
            &context.channel,
            &context.interaction_type,
            inbox_url.as_str(),
        );
        let outcome = self
            .slack
            .post_message_with_receipt(channel_id, &text, Some(&message_id.to_string()))
            .await;
        let (state, timestamp) = match classify_post_notice_attempt(channel_id, &outcome) {
            PostNoticeAttempt::Confirmed => ("notified", outcome.ok().map(|row| row.timestamp)),
            PostNoticeAttempt::RetryAfterRateLimit { .. } => ("pending", None),
            PostNoticeAttempt::Rejected => ("failed", None),
            PostNoticeAttempt::ReconcileBeforeResend => ("unknown", None),
        };
        store::inbox_alerts::record(
            &self.application_store,
            message_id,
            state,
            timestamp.as_deref(),
        )
        .await
        .map_err(WorkflowStepError::retryable)?;
        Ok(state.into())
    }
}
