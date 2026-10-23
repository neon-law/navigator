//! Durable, metadata-only Slack notifications for persisted inbound Messages.

use std::sync::Arc;

use restate_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use store::inbox::{direction, Conversation, Message};
use store::surreal::SurrealDb;
use uuid::Uuid;
use workflows::{InboundMessageAlertReport, InboundMessageAlertRequest, SlackBot};

#[derive(Clone)]
pub struct InboundMessageAlertService {
    surreal: SurrealDb,
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
        surreal: SurrealDb,
        slack: Arc<dyn SlackBot>,
        messages_channel_id: String,
        staging_messages_channel_id: String,
        simulated: bool,
        slack_enabled: bool,
        public_origin: Option<String>,
    ) -> Self {
        Self {
            surreal,
            slack,
            messages_channel_id,
            staging_messages_channel_id,
            simulated,
            slack_enabled,
            public_origin,
        }
    }

    #[cfg(test)]
    fn channel_id(&self) -> &str {
        if self.simulated {
            &self.staging_messages_channel_id
        } else {
            &self.messages_channel_id
        }
    }
}

#[cfg(test)]
mod tests {
    use super::alert_context;
    use store::inbox::{self, IngestInbound};

    #[tokio::test]
    async fn alert_context_includes_only_persisted_inbound_metadata() {
        let db = store::test_support::mem_surreal().await;
        let firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let incoming = inbox::ingest_inbound(
            &db,
            &IngestInbound {
                firm_id: firm,
                brand_key: Some("neon"),
                interaction_type: Some("comment"),
                provider_post_id: Some("post-synthetic"),
                parent_provider_message_id: None,
                visibility: Some("public"),
                channel: inbox::channel::INSTAGRAM,
                provider_account: "synthetic-account",
                provider_message_id: "synthetic-comment",
                provider_thread_ref: Some("synthetic-thread"),
                external_address: "synthetic-sender",
                external_name: Some("Synthetic sender"),
                subject: None,
                body: "private message body must not be sent to Slack",
                source_page: None,
                source_channel: Some(inbox::channel::INSTAGRAM),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachment_asset_id: None,
                occurred_at: "2026-10-07T12:00:00Z",
            },
        )
        .await
        .unwrap();

        let context = alert_context(&db, incoming.message_id)
            .await
            .unwrap()
            .expect("inbound message should alert");
        assert_eq!(context.brand_key, "neon");
        assert_eq!(context.channel, "instagram");
        assert_eq!(context.interaction_type, "comment");

        db.query("UPDATE $id SET direction = 'internal'")
            .bind((
                "id",
                store::surreal::record_id("message", incoming.message_id),
            ))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(alert_context(&db, incoming.message_id)
            .await
            .unwrap()
            .is_none());
    }

    async fn synthetic_inbound(
        db: &store::surreal::SurrealDb,
        firm_id: uuid::Uuid,
        brand: &str,
    ) -> inbox::IngestedInbound {
        inbox::ingest_inbound(
            db,
            &IngestInbound {
                firm_id,
                brand_key: Some(brand),
                interaction_type: Some("message"),
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: Some("private"),
                channel: "email",
                provider_account: "synthetic-account",
                provider_message_id: "synthetic-id",
                provider_thread_ref: None,
                external_address: "sender@example.com",
                external_name: None,
                subject: None,
                body: "Synthetic confidential body",
                source_page: None,
                source_channel: None,
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachment_asset_id: None,
                occurred_at: "2026-10-10T00:00:00Z",
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn persisted_receipts_prevent_duplicate_posts_and_cross_firm_notices() {
        let db = store::test_support::mem_surreal().await;
        let firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let other = store::test_support::seed_firm_with_brand(&db, "lawyer-shook").await;
        let slack = std::sync::Arc::new(workflows::CapturingSlackBot::new());
        let service = super::InboundMessageAlertService::new(
            db.clone(),
            slack.clone(),
            "CSYNTHETIC".into(),
            "CSTAGING".into(),
            false,
            true,
            Some("https://example.com".into()),
        );
        for (firm_id, expected) in [(firm, "notified"), (other, "disabled")] {
            let incoming = synthetic_inbound(
                &db,
                firm_id,
                if firm_id == firm {
                    "neon"
                } else {
                    "lawyer-shook"
                },
            )
            .await;
            let context = alert_context(&db, incoming.message_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                service
                    .deliver_alert(incoming.message_id, &context)
                    .await
                    .unwrap(),
                expected
            );
            assert_eq!(
                service
                    .deliver_alert(incoming.message_id, &context)
                    .await
                    .unwrap(),
                expected
            );
            if firm_id == firm {
                db.query("UPDATE $id SET alert_state = 'unknown', alert_timestamp = NONE")
                    .bind((
                        "id",
                        store::surreal::record_id("message", incoming.message_id),
                    ))
                    .await
                    .unwrap()
                    .check()
                    .unwrap();
                assert_eq!(
                    service
                        .deliver_alert(incoming.message_id, &context)
                        .await
                        .unwrap(),
                    "notified"
                );
                db.query("UPDATE $id SET alert_state = 'unknown', alert_channel_id = 'CMISSING', alert_timestamp = NONE")
                    .bind(("id", store::surreal::record_id("message", incoming.message_id)))
                    .await.unwrap().check().unwrap();
                assert_eq!(
                    service
                        .deliver_alert(incoming.message_id, &context)
                        .await
                        .unwrap(),
                    "unknown"
                );
                db.query("UPDATE $id UNSET alert_state, alert_channel_id, alert_timestamp")
                    .bind((
                        "id",
                        store::surreal::record_id("message", incoming.message_id),
                    ))
                    .await
                    .unwrap()
                    .check()
                    .unwrap();
                assert_eq!(
                    service
                        .deliver_alert(incoming.message_id, &context)
                        .await
                        .unwrap(),
                    "historical"
                );
            }
        }
        let notices = slack.posted_messages();
        assert_eq!(notices.len(), 1);
        assert!(!notices[0].1.contains("confidential body"));
        assert!(notices[0].1.contains("/app/inbox/"));
    }

    #[test]
    fn journaled_context_without_the_new_firm_field_still_decodes() {
        let context: super::AlertContext = serde_json::from_value(serde_json::json!({
            "conversation_id": "00000000-0000-0000-0000-000000000001", "brand_key":"neon",
            "channel":"email", "interaction_type":"message",
        }))
        .unwrap();
        assert!(context.firm_id.is_nil());
    }

    #[tokio::test]
    async fn channel_id_uses_the_staging_lane_when_simulated() {
        let db = store::test_support::mem_surreal().await;
        let slack = std::sync::Arc::new(workflows::CapturingSlackBot::new());
        let live = super::InboundMessageAlertService::new(
            db.clone(),
            slack.clone(),
            "prod-channel".into(),
            "staging-channel".into(),
            false,
            true,
            Some("https://navigator.example".into()),
        );
        let simulated = super::InboundMessageAlertService::new(
            db,
            slack,
            "prod-channel".into(),
            "staging-channel".into(),
            true,
            true,
            None,
        );
        assert_eq!(live.channel_id(), "prod-channel");
        assert_eq!(simulated.channel_id(), "staging-channel");
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AlertContext {
    conversation_id: Uuid,
    #[serde(default)]
    firm_id: Uuid,
    brand_key: String,
    channel: String,
    interaction_type: String,
}

async fn alert_context(
    surreal: &SurrealDb,
    message_id: Uuid,
) -> Result<Option<AlertContext>, HandlerError> {
    let Some(message): Option<Message> = store::inbox::message_by_id(surreal, message_id)
        .await
        .map_err(HandlerError::from)?
    else {
        return Err(TerminalError::new("inbound message not found").into());
    };
    if message.direction != direction::INBOUND {
        return Ok(None);
    }
    let Some(conversation): Option<Conversation> =
        store::inbox::conversation_by_id(surreal, message.conversation_id)
            .await
            .map_err(HandlerError::from)?
    else {
        return Err(TerminalError::new("inbound conversation not found").into());
    };
    Ok(Some(AlertContext {
        conversation_id: conversation.id,
        firm_id: conversation.firm_id,
        brand_key: conversation.brand_key.unwrap_or_else(|| "unbranded".into()),
        channel: conversation.channel,
        interaction_type: conversation.interaction_type,
    }))
}

#[restate_sdk::workflow(name = "InboundMessageAlert")]
impl InboundMessageAlertService {
    #[restate_sdk::handler]
    async fn run(
        &self,
        ctx: WorkflowContext<'_>,
        Json(request): Json<InboundMessageAlertRequest>,
    ) -> Result<Json<InboundMessageAlertReport>, HandlerError> {
        let message_id = request.message_id;
        let context = {
            let surreal = self.surreal.clone();
            ctx.run(move || async move { Ok(Json(alert_context(&surreal, message_id).await?)) })
                .name("load-inbound-message-context")
                .await?
                .into_inner()
        };
        let Some(context) = context else {
            return Ok(Json(InboundMessageAlertReport {
                message_id,
                state: "skipped_non_inbound".into(),
            }));
        };
        let service = self.clone();
        ctx.run(move || async move {
            service.deliver_alert(message_id, &context).await?;
            Ok(Json(()))
        })
        .name("post-inbound-message-alert")
        .await?;
        let surreal = self.surreal.clone();
        let state = ctx
            .run(move || async move {
                let receipt = store::inbox_alerts::receipt(&surreal, message_id).await?;
                Ok(Json(receipt.state.unwrap_or_else(|| "historical".into())))
            })
            .name("read-inbound-message-alert-receipt")
            .await?
            .into_inner();
        Ok(Json(InboundMessageAlertReport { message_id, state }))
    }
}

impl InboundMessageAlertService {
    async fn reconcile_unknown_alert(
        &self,
        message_id: Uuid,
        channel_id: Option<&str>,
    ) -> Result<String, HandlerError> {
        if let Some(channel) = channel_id {
            if let Ok(Some(found)) = self
                .slack
                .message_receipt_by_correlation(channel, &message_id.to_string())
                .await
            {
                if found.channel_id == channel && !found.timestamp.is_empty() {
                    store::inbox_alerts::record(
                        &self.surreal,
                        message_id,
                        "notified",
                        Some(&found.timestamp),
                    )
                    .await?;
                    return Ok("notified".into());
                }
            }
        }
        store::inbox_alerts::record(&self.surreal, message_id, "unknown", None).await?;
        Ok("unknown".into())
    }

    async fn deliver_alert(
        &self,
        message_id: Uuid,
        _context: &AlertContext,
    ) -> Result<String, HandlerError> {
        let Some(context) = alert_context(&self.surreal, message_id).await? else {
            return Ok("skipped_non_inbound".into());
        };
        let receipt = store::inbox_alerts::receipt(&self.surreal, message_id).await?;
        if receipt.state.as_deref() == Some("unknown") {
            return self
                .reconcile_unknown_alert(message_id, receipt.channel_id.as_deref())
                .await;
        }
        if receipt.state.as_deref() != Some("pending") {
            return Ok(receipt.state.unwrap_or_else(|| "historical".into()));
        }
        // A deployment-wide channel has one owning Firm. Never route another
        // Firm's inbound metadata into it. Invalid explicit configuration fails closed.
        let owning_firm = match std::env::var("SLACK_MESSAGES_FIRM_ID").ok() {
            Some(value) => Uuid::parse_str(&value).ok(),
            None => store::firms::firm_id_for_brand_key(&self.surreal, "neon").await?,
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
            store::inbox_alerts::record(&self.surreal, message_id, "disabled", None).await?;
            return Ok("disabled".into());
        };
        inbox_url.set_path(&format!("/app/inbox/{}", context.conversation_id));
        inbox_url.set_query(None);
        inbox_url.set_fragment(None);
        if !store::inbox_alerts::reserve(&self.surreal, message_id, channel_id).await? {
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
        let (state, timestamp) = match outcome {
            Ok(receipt) if receipt.channel_id == channel_id && !receipt.timestamp.is_empty() => {
                ("notified", Some(receipt.timestamp))
            }
            Err(workflows::SlackBotError::RateLimited { .. }) => ("pending", None),
            Err(workflows::SlackBotError::Api(ref code))
                if matches!(
                    code.as_str(),
                    "request_timeout" | "internal_error" | "fatal_error"
                ) =>
            {
                ("unknown", None)
            }
            Err(
                workflows::SlackBotError::Api(_)
                | workflows::SlackBotError::MissingScope { .. }
                | workflows::SlackBotError::HttpStatus(400 | 401 | 403 | 404),
            ) => ("failed", None),
            _ => ("unknown", None),
        };
        store::inbox_alerts::record(&self.surreal, message_id, state, timestamp.as_deref()).await?;
        Ok(state.into())
    }
}
