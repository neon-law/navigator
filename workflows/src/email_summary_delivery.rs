//! Bounded Slack rendering and delivery orchestration for email summaries.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::email_summary::EmailSummary;
use crate::notify::{SlackBot, SlackBotError, SlackMessageReceipt};

const MAX_SLACK_CHARS: usize = 39_000;
const MAX_PROVIDER_CHARS: usize = 6_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderDelivery {
    Succeeded {
        model: String,
        result: EmailSummary,
    },
    Failed {
        model: String,
        status: String,
    },
    /// No model was called: the sender gate refused the message.
    Skipped {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryDeliveryMessage {
    pub receipt_id: Uuid,
    pub gemini: ProviderDelivery,
    pub original_message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryResult {
    Sent(SlackMessageReceipt),
    AlreadyConfirmed,
    Unknown,
}

#[derive(Debug, Error)]
pub enum SummaryDeliveryError {
    #[error("delivery store: {0}")]
    Store(#[from] store::email_deliveries::EmailDeliveryError),
    #[error("Slack delivery: {0}")]
    Slack(#[from] SlackBotError),
}

#[must_use]
pub fn render_summary_message(message: &SummaryDeliveryMessage) -> String {
    let mut output = format!("Support summary receipt: {}\n", message.receipt_id);
    output.push_str(&truncate(
        &render_provider("Gemini", &message.gemini),
        MAX_PROVIDER_CHARS,
    ));
    truncate(&output, MAX_SLACK_CHARS)
}

/// The full message body, posted as a thread reply under the summary. `None`
/// when there is no body to show.
#[must_use]
pub fn render_original_message(message: &SummaryDeliveryMessage) -> Option<String> {
    let body = message.original_message.trim();
    (!body.is_empty()).then(|| truncate(&escape(body), MAX_SLACK_CHARS))
}

fn render_provider(label: &str, provider: &ProviderDelivery) -> String {
    match provider {
        ProviderDelivery::Succeeded { model, result } => format!(
            "*{label}* ({})\nSummary: {}\nRequested actions: {}\nSender-stated dates: {}\nMissing information: {}",
            escape(model),
            escape(&result.summary),
            render_list(&result.requested_actions),
            render_list(&result.sender_stated_dates),
            render_list(&result.missing_information),
        ),
        ProviderDelivery::Failed { model, status } => format!(
            "*{label}* ({})\nFailure: {}",
            escape(model),
            escape(status),
        ),
        ProviderDelivery::Skipped { reason } => {
            format!("*{label}* skipped\nReason: {}", escape(reason))
        }
    }
}

fn render_list(values: &[String]) -> String {
    if values.is_empty() {
        return "(none)".to_string();
    }
    values
        .iter()
        .map(|value| format!("• {}", escape(value)))
        .collect::<Vec<_>>()
        .join("; ")
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max_chars.saturating_sub(15)).collect();
    truncated.push_str("\n[truncated]");
    truncated
}

/// Send one rendered summary through the configured private channel. The
/// store admission is the idempotency boundary; unknown external outcomes
/// stop replay until an operator reconciles them.
pub async fn deliver_summary(
    db: &store::surreal::SurrealDb,
    slack: Arc<dyn SlackBot>,
    receipt_id: Uuid,
    channel_id: &str,
    message: &SummaryDeliveryMessage,
) -> Result<DeliveryResult, SummaryDeliveryError> {
    store::email_deliveries::ensure(db, receipt_id, channel_id).await?;
    let correlation_id = format!("summary-{receipt_id}");
    let admitted = store::email_deliveries::admit_attempt(db, receipt_id, &correlation_id).await?;
    if admitted.delivery.state == store::email_deliveries::CONFIRMED {
        return Ok(DeliveryResult::AlreadyConfirmed);
    }
    if admitted.attempt_id.is_none() {
        // A `sending` row means an earlier run admitted the post and never
        // recorded its outcome. Name it `unknown` so reconciliation lists it.
        if admitted.delivery.state == store::email_deliveries::SENDING {
            store::email_deliveries::mark_unknown(
                db,
                receipt_id,
                "interrupted before the Slack outcome was recorded",
            )
            .await?;
        }
        return Ok(DeliveryResult::Unknown);
    }

    let started = std::time::Instant::now();
    let rendered = render_summary_message(message);
    match slack
        .post_message_with_receipt(channel_id, &rendered, Some(&correlation_id))
        .await
    {
        Ok(receipt) => {
            store::email_deliveries::mark_confirmed(
                db,
                receipt_id,
                &receipt.channel_id,
                &receipt.timestamp,
            )
            .await?;
            telemetry::record_email_stage(
                &receipt_id.to_string(),
                telemetry::EmailStage::Delivery,
                telemetry::EmailOutcome::Confirmed,
                started.elapsed(),
            );
            if let Ok(Some(receipt)) = store::email_receipts::find_by_id(db, receipt_id).await {
                telemetry::record_email_delivery_latency(
                    (chrono::Utc::now() - receipt.inserted_at)
                        .to_std()
                        .unwrap_or_default()
                        .as_secs_f64(),
                );
            }
            // The summary is confirmed; the body is a courtesy copy and the
            // archive still holds it, so a failed reply is logged, not retried.
            if let Some(body) = render_original_message(message) {
                if let Err(error) = slack
                    .post_thread_reply(&receipt.channel_id, &receipt.timestamp, &body)
                    .await
                {
                    tracing::warn!(%receipt_id, %error, "summary thread reply failed");
                }
            }
            Ok(DeliveryResult::Sent(receipt))
        }
        Err(
            error @ (SlackBotError::Transport(_)
            | SlackBotError::RetryableHttpStatus(_)
            | SlackBotError::IncompleteResponse),
        ) => {
            store::email_deliveries::mark_unknown(db, receipt_id, &error.to_string()).await?;
            telemetry::record_email_stage(
                &receipt_id.to_string(),
                telemetry::EmailStage::Delivery,
                telemetry::EmailOutcome::Unknown,
                started.elapsed(),
            );
            Err(error.into())
        }
        Err(
            error @ (SlackBotError::RateLimited { .. }
            | SlackBotError::HttpStatus(_)
            | SlackBotError::Api(_)
            | SlackBotError::MissingScope { .. }),
        ) => {
            store::email_deliveries::mark_failed(db, receipt_id, &error.to_string()).await?;
            telemetry::record_email_stage(
                &receipt_id.to_string(),
                telemetry::EmailStage::Delivery,
                telemetry::EmailOutcome::Failed,
                started.elapsed(),
            );
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;

    fn message(receipt_id: Uuid) -> SummaryDeliveryMessage {
        SummaryDeliveryMessage {
            receipt_id,
            original_message: "Please review the attached draft.".to_string(),
            gemini: ProviderDelivery::Succeeded {
                model: "gemini-test".to_string(),
                result: EmailSummary {
                    summary: "The sender asks for an update <@U123>.".to_string(),
                    requested_actions: vec!["Review & reply".to_string()],
                    sender_stated_dates: vec!["2026-09-21".to_string()],
                    missing_information: vec![],
                },
            },
        }
    }

    #[test]
    fn renders_the_gemini_summary_and_escapes_slack_markup() {
        let rendered = render_summary_message(&message(Uuid::nil()));
        assert!(rendered.contains("*Gemini* (gemini-test)"));
        assert!(!rendered.contains("Claude"));
        assert!(rendered.contains("&lt;@U123&gt;"));
        assert!(rendered.contains("Review &amp; reply"));
        assert!(!rendered.contains("Please review the attached draft."));
        assert_eq!(
            render_original_message(&message(Uuid::nil())).as_deref(),
            Some("Please review the attached draft.")
        );
        assert!(!rendered.contains("<@U123>"));
    }

    #[test]
    fn renders_a_bounded_provider_failure_as_status_only() {
        let rendered = render_summary_message(&SummaryDeliveryMessage {
            receipt_id: Uuid::nil(),
            original_message: "The original request.".to_string(),
            gemini: ProviderDelivery::Failed {
                model: "gemini-test".to_string(),
                status: "provider timeout".to_string(),
            },
        });
        assert!(rendered.contains("*Gemini* (gemini-test)\nFailure: provider timeout"));
        assert!(!rendered.contains("The original request."));
    }

    #[test]
    fn the_thread_reply_is_the_escaped_body_and_absent_when_empty() {
        let mut message = message(Uuid::nil());
        message.original_message = "  Who are you <b>?  ".to_string();
        assert_eq!(
            render_original_message(&message).as_deref(),
            Some("Who are you &lt;b&gt;?")
        );
        message.original_message = " \n".to_string();
        assert_eq!(render_original_message(&message), None);
    }

    #[test]
    fn renders_a_skipped_summary_with_the_original_message() {
        let rendered = render_summary_message(&SummaryDeliveryMessage {
            receipt_id: Uuid::nil(),
            original_message: "Who are you <b>?".to_string(),
            gemini: ProviderDelivery::Skipped {
                reason: "sender stranger@example.com is not a known person".to_string(),
            },
        });
        assert!(rendered.contains(
            "*Gemini* skipped\nReason: sender stranger@example.com is not a known person"
        ));
        assert!(!rendered.contains("Who are you"));
    }

    #[test]
    fn renderer_bounds_large_provider_text() {
        let mut large = message(Uuid::nil());
        if let ProviderDelivery::Succeeded { result, .. } = &mut large.gemini {
            result.summary = "x".repeat(10_000);
        }
        let rendered = render_summary_message(&large);
        assert!(rendered.chars().count() <= MAX_SLACK_CHARS);
        assert!(rendered.contains("[truncated]"));
    }

    #[derive(Clone, Copy)]
    enum TestSlackOutcome {
        Incomplete,
        Api,
    }

    #[derive(Clone)]
    struct TestSlackBot {
        posts: Arc<AtomicUsize>,
        outcome: TestSlackOutcome,
    }

    impl TestSlackBot {
        fn incomplete() -> Self {
            Self {
                posts: Arc::new(AtomicUsize::new(0)),
                outcome: TestSlackOutcome::Incomplete,
            }
        }

        fn api() -> Self {
            Self {
                posts: Arc::new(AtomicUsize::new(0)),
                outcome: TestSlackOutcome::Api,
            }
        }
    }

    #[async_trait]
    impl SlackBot for TestSlackBot {
        async fn create_private_channel(
            &self,
            _name: &str,
        ) -> Result<crate::notify::SlackChannel, SlackBotError> {
            Ok(crate::notify::SlackChannel {
                id: "C-test".to_string(),
                name: "test".to_string(),
            })
        }

        async fn post_message(&self, _channel_id: &str, _text: &str) -> Result<(), SlackBotError> {
            Ok(())
        }

        async fn user_id_by_email(&self, _email: &str) -> Result<Option<String>, SlackBotError> {
            Ok(None)
        }

        async fn channel_member_ids(
            &self,
            _channel_id: &str,
        ) -> Result<Vec<String>, SlackBotError> {
            Ok(Vec::new())
        }

        async fn invite_to_channel(
            &self,
            _channel_id: &str,
            _user_ids: &[String],
        ) -> Result<(), SlackBotError> {
            Ok(())
        }

        async fn post_message_with_receipt(
            &self,
            _channel_id: &str,
            _text: &str,
            _correlation_id: Option<&str>,
        ) -> Result<SlackMessageReceipt, SlackBotError> {
            self.posts.fetch_add(1, Ordering::SeqCst);
            Err(match self.outcome {
                TestSlackOutcome::Incomplete => SlackBotError::IncompleteResponse,
                TestSlackOutcome::Api => SlackBotError::Api("channel_not_found".to_string()),
            })
        }
    }

    async fn setup_receipt() -> (store::surreal::SurrealDb, Uuid) {
        let db = store::test_support::mem_surreal().await;
        let receipt_id = Uuid::now_v7();
        store::email_receipts::ensure(
            &db,
            &store::email_receipts::NewEmailReceipt {
                receiving_mailbox: "support@example.com",
                deployment: "staging",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: &receipt_id.to_string(),
                source_message_id: None,
                archive_key: "inbound/example.eml",
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .expect("receipt setup");
        (db, receipt_id)
    }

    #[tokio::test]
    async fn a_send_interrupted_before_its_outcome_is_recorded_becomes_unknown_without_a_post() {
        let (db, receipt_id) = setup_receipt().await;
        store::email_deliveries::ensure(&db, receipt_id, "C-test")
            .await
            .expect("delivery");
        // A predecessor admitted the attempt and died before recording a result.
        store::email_deliveries::admit_attempt(&db, receipt_id, "summary-crashed")
            .await
            .expect("admit");
        let slack = Arc::new(TestSlackBot::api());
        let result = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await
        .expect("an interrupted send is held, not an error");

        assert_eq!(result, DeliveryResult::Unknown);
        assert_eq!(slack.posts.load(Ordering::SeqCst), 0, "nothing is resent");
        let delivery = store::email_deliveries::find(&db, receipt_id)
            .await
            .expect("delivery lookup")
            .expect("delivery exists");
        assert_eq!(delivery.state, store::email_deliveries::UNKNOWN);
        assert!(
            store::email_deliveries::list_by_state(&db, store::email_deliveries::UNKNOWN)
                .await
                .expect("list")
                .iter()
                .any(|delivery| delivery.receipt_id == receipt_id)
        );
    }

    #[tokio::test]
    async fn incomplete_slack_success_is_unknown_and_not_replayed() {
        let (db, receipt_id) = setup_receipt().await;
        let slack = Arc::new(TestSlackBot::incomplete());
        let result = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await;

        assert!(matches!(
            result,
            Err(SummaryDeliveryError::Slack(
                SlackBotError::IncompleteResponse
            ))
        ));
        let delivery = store::email_deliveries::find(&db, receipt_id)
            .await
            .expect("delivery lookup")
            .expect("delivery exists");
        assert_eq!(delivery.state, store::email_deliveries::UNKNOWN);
        assert_eq!(
            store::email_deliveries::attempts(&db, receipt_id)
                .await
                .expect("attempt lookup")
                .len(),
            1
        );

        let replay = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await
        .expect("unknown delivery is held");
        assert_eq!(replay, DeliveryResult::Unknown);
        assert_eq!(slack.posts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn permanent_slack_failure_is_failed_and_retried() {
        let (db, receipt_id) = setup_receipt().await;
        let slack = Arc::new(TestSlackBot::api());
        let first = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await;
        assert!(matches!(
            first,
            Err(SummaryDeliveryError::Slack(SlackBotError::Api(_)))
        ));
        assert_eq!(
            store::email_deliveries::find(&db, receipt_id)
                .await
                .expect("delivery lookup")
                .expect("delivery exists")
                .state,
            store::email_deliveries::FAILED
        );

        let second = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await;
        assert!(matches!(
            second,
            Err(SummaryDeliveryError::Slack(SlackBotError::Api(_)))
        ));
        assert_eq!(slack.posts.load(Ordering::SeqCst), 2);
        assert_eq!(
            store::email_deliveries::attempts(&db, receipt_id)
                .await
                .expect("attempt lookup")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn confirmed_delivery_replays_without_posting_again() {
        let (db, receipt_id) = setup_receipt().await;
        let slack = Arc::new(crate::notify::CapturingSlackBot::new());
        let first = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await
        .expect("delivery succeeds");
        assert!(matches!(first, DeliveryResult::Sent(_)));

        let replay = deliver_summary(
            &db,
            slack.clone(),
            receipt_id,
            "C-test",
            &message(receipt_id),
        )
        .await
        .expect("confirmed replay is held");
        assert_eq!(replay, DeliveryResult::AlreadyConfirmed);
        assert_eq!(slack.posted_messages().len(), 1);
    }
}
