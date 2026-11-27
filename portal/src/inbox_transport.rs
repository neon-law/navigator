//! Outbound adapters for the Firm inbox.
//!
//! Email goes through the existing [`crate::email::EmailService`], which is
//! SendGrid in production and a capturing double everywhere else. Twilio SMS
//! replies use `crate::inbox_sms`; fixture transports remain injectable. A
//! timeout is `unknown` and is not retried by this adapter.

use std::sync::Arc;

use async_trait::async_trait;
use store::application_store::ApplicationStore;
use store::config::SocialEgressGate;
use store::inbox::{channel, OutboundEnvelope, OutboundTransport, TransportOutcome};
use store::social_accounts::SocialProvider;

use crate::email::{EmailError, EmailService, OutboundEmail};

/// Sends an email envelope through [`EmailService`].
pub struct EmailTransport {
    email: Arc<dyn EmailService>,
}

impl EmailTransport {
    #[must_use]
    pub fn new(email: Arc<dyn EmailService>) -> Self {
        Self { email }
    }
}

#[async_trait]
impl OutboundTransport for EmailTransport {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        if envelope.channel != channel::EMAIL {
            return Err("email transport received another channel".into());
        }
        let subject = envelope
            .subject
            .clone()
            .unwrap_or_else(|| "Message from the firm".into());
        match self
            .email
            .send(OutboundEmail::new(&envelope.to, subject, &envelope.body))
            .await
        {
            Ok(receipt) => Ok(TransportOutcome::Submitted {
                provider_ref: receipt
                    .message_id
                    .unwrap_or_else(|| "email-submitted".into()),
            }),
            Err(error) if transport_is_unknown(&error) => Ok(TransportOutcome::Unknown),
            Err(error) => Ok(TransportOutcome::Rejected {
                detail: error.to_string(),
            }),
        }
    }
}

fn transport_is_unknown(error: &EmailError) -> bool {
    match error {
        EmailError::Transport(detail) => {
            let detail = detail.to_ascii_lowercase();
            detail.contains("timeout") || detail.contains("timed out")
        }
        EmailError::InvalidRecipient(_) => false,
    }
}

/// Routes each envelope to the adapter for its channel.
pub struct ChannelTransport {
    email: EmailTransport,
    sms: Arc<dyn OutboundTransport>,
    social: Arc<dyn OutboundTransport>,
}

impl ChannelTransport {
    #[must_use]
    pub fn new(email: Arc<dyn EmailService>, sms: Arc<dyn OutboundTransport>) -> Self {
        Self {
            email: EmailTransport::new(email),
            sms,
            social: Arc::new(UnconfiguredSocial),
        }
    }

    #[must_use]
    pub fn with_social(mut self, social: Arc<dyn OutboundTransport>) -> Self {
        self.social = social;
        self
    }
}

#[async_trait]
impl OutboundTransport for ChannelTransport {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        match envelope.channel.as_str() {
            channel::EMAIL => self.email.deliver(envelope).await,
            channel::SMS => self.sms.deliver(envelope).await,
            channel::FACEBOOK
            | channel::INSTAGRAM
            | channel::THREADS
            | channel::YOUTUBE
            | channel::TIKTOK => self.social.deliver(envelope).await,
            other => Err(format!("unsupported inbox channel {other}")),
        }
    }
}

/// Replies through a connected Brand social account. Credentials are
/// decrypted only for the provider request and are never included in errors.
pub struct SocialReplyTransport {
    application_store: ApplicationStore,
    kms: Arc<dyn cloud::RuntimeKms>,
    egress: SocialEgressGate,
    refresh: workflows::social_publish::TokenRefreshConfig,
    http: reqwest::Client,
    meta_graph_base: String,
    threads_graph_base: String,
    youtube_comments_url: String,
    tiktok_base: String,
}

impl SocialReplyTransport {
    #[must_use]
    pub fn new(application_store: ApplicationStore, kms: Arc<dyn cloud::RuntimeKms>) -> Self {
        Self {
            application_store,
            kms,
            egress: SocialEgressGate::from_process_env(),
            refresh: workflows::social_publish::TokenRefreshConfig::from_env(),
            http: reqwest::Client::new(),
            meta_graph_base: "https://graph.facebook.com/v26.0".into(),
            threads_graph_base: "https://graph.threads.net/v1.0".into(),
            youtube_comments_url: "https://www.googleapis.com/youtube/v3/comments".into(),
            tiktok_base: "https://business-api.tiktok.com/open_api/v1.3".into(),
        }
    }

    async fn send_threads(
        &self,
        envelope: &OutboundEnvelope,
        access_token: &str,
    ) -> Result<TransportOutcome, String> {
        if envelope.visibility.as_deref() != Some("public")
            || envelope.interaction_type == "direct_message"
        {
            return Ok(rejected("Threads replies must be public"));
        }
        let Some(reply_to) = envelope.reply_to_provider_message_id.as_deref() else {
            return Ok(rejected("Threads reply has no parent post reference"));
        };
        let create = self
            .http
            .post(format!("{}/me/threads", self.threads_graph_base))
            .form(&[
                ("media_type", "TEXT"),
                ("text", envelope.body.as_str()),
                ("reply_to_id", reply_to),
                ("access_token", access_token),
            ])
            .send()
            .await;
        let Ok(create_response) = create else {
            return Ok(TransportOutcome::Unknown);
        };
        let create_status = create_response.status();
        let create_body = match create_response.bytes().await {
            Ok(body) if body.len() <= 64 * 1024 => body,
            _ => return Ok(TransportOutcome::Unknown),
        };
        if !create_status.is_success() {
            return Ok(classify_social_response(create_status, &create_body));
        }
        let Ok(container) = serde_json::from_slice::<serde_json::Value>(&create_body) else {
            return Ok(TransportOutcome::Unknown);
        };
        let Some(container_id) = container.get("id").and_then(serde_json::Value::as_str) else {
            return Ok(TransportOutcome::Unknown);
        };
        let publish = self
            .http
            .post(format!("{}/me/threads_publish", self.threads_graph_base))
            .form(&[
                ("creation_id", container_id),
                ("access_token", access_token),
            ])
            .send()
            .await;
        self.outcome(publish).await
    }

    async fn send_meta(
        &self,
        envelope: &OutboundEnvelope,
        provider: SocialProvider,
        access_token: &str,
    ) -> Result<TransportOutcome, String> {
        let Some(reply_to) = envelope.reply_to_provider_message_id.as_deref() else {
            return Ok(rejected("social reply has no provider message reference"));
        };
        let (url, form) = if envelope.interaction_type == "direct_message" {
            if envelope.visibility.as_deref() != Some("private") {
                return Ok(rejected("direct messages must remain private"));
            }
            (
                format!("{}/{}/messages", self.meta_graph_base, envelope.account),
                vec![
                    ("recipient[id]", envelope.to.as_str()),
                    ("message[text]", envelope.body.as_str()),
                    ("access_token", access_token),
                ],
            )
        } else {
            let edge = match (provider, envelope.visibility.as_deref()) {
                (SocialProvider::Facebook, Some("public")) => "comments",
                (SocialProvider::Facebook | SocialProvider::Instagram, Some("private")) => {
                    "private_replies"
                }
                (SocialProvider::Instagram, Some("public")) => "replies",
                _ => return Ok(rejected("provider does not support this reply audience")),
            };
            (
                format!("{}/{reply_to}/{edge}", self.meta_graph_base),
                vec![
                    ("message", envelope.body.as_str()),
                    ("access_token", access_token),
                ],
            )
        };
        self.post_form(&url, &form).await
    }

    async fn send_youtube(
        &self,
        envelope: &OutboundEnvelope,
        access_token: &str,
    ) -> Result<TransportOutcome, String> {
        if envelope.visibility.as_deref() != Some("public")
            || envelope.interaction_type == "direct_message"
        {
            return Ok(rejected("YouTube replies must be public comment replies"));
        }
        let Some(parent_id) = envelope.reply_to_provider_message_id.as_deref() else {
            return Ok(rejected("YouTube reply has no parent comment reference"));
        };
        let response = self
            .http
            .post(&self.youtube_comments_url)
            .bearer_auth(access_token)
            .query(&[("part", "snippet")])
            .json(&serde_json::json!({
                "snippet": { "parentId": parent_id, "textOriginal": envelope.body }
            }))
            .send()
            .await;
        self.outcome(response).await
    }

    async fn post_form(
        &self,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<TransportOutcome, String> {
        self.outcome(self.http.post(url).form(form).send().await)
            .await
    }

    async fn outcome(
        &self,
        response: Result<reqwest::Response, reqwest::Error>,
    ) -> Result<TransportOutcome, String> {
        let Ok(response) = response else {
            return Ok(TransportOutcome::Unknown);
        };
        let status = response.status();
        let body = match response.bytes().await {
            Ok(body) if body.len() <= 64 * 1024 => body,
            _ => return Ok(TransportOutcome::Unknown),
        };
        Ok(classify_social_response(status, &body))
    }
}

fn classify_social_response(status: reqwest::StatusCode, body: &[u8]) -> TransportOutcome {
    if status.is_success() {
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
            return TransportOutcome::Unknown;
        };
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| value.get("message_id").and_then(serde_json::Value::as_str));
        return match id {
            Some(provider_ref) => TransportOutcome::Submitted {
                provider_ref: provider_ref.to_string(),
            },
            None => TransportOutcome::Unknown,
        };
    }
    if status.is_client_error() {
        let detail = serde_json::from_slice::<serde_json::Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| format!("social provider rejected the reply ({status})"));
        return rejected(&detail.chars().take(300).collect::<String>());
    }
    TransportOutcome::Unknown
}

/// Whether a TikTok connection may reply to a comment. The reply needs both
/// grants: `comment.list` to read the parent comment's visibility before
/// answering it, and `comment.list.manage` to post the answer.
fn tiktok_can_reply(scopes: &[String]) -> bool {
    ["comment.list", "comment.list.manage"]
        .iter()
        .all(|required| scopes.iter().any(|scope| scope == required))
}

fn rejected(detail: &str) -> TransportOutcome {
    TransportOutcome::Rejected {
        detail: detail.to_string(),
    }
}

#[async_trait]
impl OutboundTransport for SocialReplyTransport {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        let provider = match envelope.channel.as_str() {
            channel::FACEBOOK => SocialProvider::Facebook,
            channel::INSTAGRAM => SocialProvider::Instagram,
            channel::THREADS => SocialProvider::Threads,
            channel::YOUTUBE => SocialProvider::Youtube,
            channel::TIKTOK => SocialProvider::Tiktok,
            _ => return Err("social transport received a non-social channel".into()),
        };
        // Checked before any provider request, including a token refresh.
        if let Err(denied) = self.egress.check() {
            return Ok(rejected(&denied.to_string()));
        }
        let accounts = store::social_accounts::for_provider_identity(
            &self.application_store,
            provider,
            &envelope.account,
        )
        .await
        .map_err(|_| "social account lookup failed".to_string())?;
        if accounts.len() != 1 {
            return Ok(rejected("social account is missing or ambiguous"));
        }
        if provider == SocialProvider::Tiktok && !tiktok_can_reply(&accounts[0].scopes) {
            return Ok(rejected(
                "Reconnect TikTok with comment.list and comment.list.manage permissions to reply",
            ));
        }
        let credential = match workflows::social_publish::send_credential(
            &self.application_store,
            self.kms.as_ref(),
            &self.http,
            &self.refresh,
            &accounts[0].brand_key,
            provider,
            &envelope.account,
        )
        .await
        {
            Ok(credential) => credential,
            Err(failure) => return Ok(rejected(&failure.to_string())),
        };
        self.send_with_token(envelope, provider, credential.access_token())
            .await
    }
}

impl SocialReplyTransport {
    async fn send_with_token(
        &self,
        envelope: &OutboundEnvelope,
        provider: SocialProvider,
        access_token: &str,
    ) -> Result<TransportOutcome, String> {
        if provider == SocialProvider::Tiktok {
            return crate::tiktok_comments::send_reply(
                &self.http,
                &self.tiktok_base,
                envelope,
                access_token,
            )
            .await;
        }
        if provider == SocialProvider::Youtube {
            return self.send_youtube(envelope, access_token).await;
        }
        if provider == SocialProvider::Threads {
            return self.send_threads(envelope, access_token).await;
        }
        self.send_meta(envelope, provider, access_token).await
    }
}

/// Fail-closed transport retained when a caller has no KMS or social config.
pub struct UnconfiguredSocial;

#[async_trait]
impl OutboundTransport for UnconfiguredSocial {
    async fn deliver(&self, _envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        Ok(rejected("social provider transport is not configured"))
    }
}

/// SMS outcome used when a deployment has not attached a carrier.
pub struct UnconfiguredSms;

#[async_trait]
impl OutboundTransport for UnconfiguredSms {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        let _ = envelope;
        Ok(TransportOutcome::Rejected {
            detail: "sms transport is not configured".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use store::inbox::channel;

    use super::*;
    use crate::email::{EmailError, EmailService, OutboundEmail, SendReceipt};

    enum ScriptedSend {
        Submitted(SendReceipt),
        Timeout,
    }

    struct ScriptedEmail {
        result: ScriptedSend,
        sent: Mutex<Vec<OutboundEmail>>,
    }

    #[async_trait]
    impl EmailService for ScriptedEmail {
        async fn send(&self, email: OutboundEmail) -> Result<SendReceipt, EmailError> {
            self.sent.lock().expect("sent").push(email);
            match &self.result {
                ScriptedSend::Submitted(receipt) => Ok(receipt.clone()),
                ScriptedSend::Timeout => Err(EmailError::Transport("request timeout".into())),
            }
        }
    }

    struct FixtureSms {
        outcome: TransportOutcome,
        calls: Arc<Mutex<Vec<OutboundEnvelope>>>,
    }

    #[async_trait]
    impl OutboundTransport for FixtureSms {
        async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
            self.calls.lock().expect("calls").push(envelope.clone());
            Ok(self.outcome.clone())
        }
    }

    fn envelope(channel_name: &str, body: &str) -> OutboundEnvelope {
        OutboundEnvelope {
            channel: channel_name.into(),
            account: "fixture-account".into(),
            interaction_type: "comment".into(),
            provider_post_id: None,
            reply_to_provider_message_id: None,
            provider_thread_ref: None,
            to: if channel_name == channel::SMS {
                "sms:fixture-sender".into()
            } else {
                "sender@example.com".into()
            },
            subject: Some("Question".into()),
            body: body.into(),
            purpose: store::inbox::purpose::MATTER.into(),
            visibility: None,
        }
    }

    #[test]
    fn social_outcomes_require_provider_receipt_and_do_not_retry_ambiguous_responses() {
        assert_eq!(
            classify_social_response(reqwest::StatusCode::OK, br#"{"id":"provider-reply-1"}"#),
            TransportOutcome::Submitted {
                provider_ref: "provider-reply-1".into(),
            }
        );
        assert_eq!(
            classify_social_response(reqwest::StatusCode::OK, br#"{"success":true}"#),
            TransportOutcome::Unknown
        );
        assert_eq!(
            classify_social_response(
                reqwest::StatusCode::BAD_REQUEST,
                br#"{"error":{"message":"invalid reply"}}"#
            ),
            TransportOutcome::Rejected {
                detail: "invalid reply".into(),
            }
        );
        assert_eq!(
            classify_social_response(reqwest::StatusCode::BAD_GATEWAY, b""),
            TransportOutcome::Unknown
        );
    }

    #[tokio::test]
    async fn email_timeout_is_unknown_and_does_not_claim_a_provider_ref() {
        let email = Arc::new(ScriptedEmail {
            result: ScriptedSend::Timeout,
            sent: Mutex::new(Vec::new()),
        });
        let outcome = EmailTransport::new(email.clone())
            .deliver(&envelope(channel::EMAIL, "We received your message."))
            .await
            .unwrap();
        assert_eq!(outcome, TransportOutcome::Unknown);
        assert_eq!(email.sent.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn channel_transport_keeps_email_and_sms_apart() {
        let email = Arc::new(ScriptedEmail {
            result: ScriptedSend::Submitted(SendReceipt {
                message_id: Some("email-fixture-1".into()),
            }),
            sent: Mutex::new(Vec::new()),
        });
        let sms_calls = Arc::new(Mutex::new(Vec::new()));
        let sms = Arc::new(FixtureSms {
            outcome: TransportOutcome::Submitted {
                provider_ref: "sms-fixture-1".into(),
            },
            calls: sms_calls.clone(),
        });
        let transport = ChannelTransport::new(email.clone(), sms.clone());
        let email_outcome = transport
            .deliver(&envelope(channel::EMAIL, "Email reply."))
            .await
            .unwrap();
        let sms_outcome = transport
            .deliver(&envelope(channel::SMS, "SMS reply."))
            .await
            .unwrap();
        assert_eq!(
            email_outcome,
            TransportOutcome::Submitted {
                provider_ref: "email-fixture-1".into(),
            }
        );
        assert_eq!(
            sms_outcome,
            TransportOutcome::Submitted {
                provider_ref: "sms-fixture-1".into(),
            }
        );
        assert_eq!(email.sent.lock().unwrap()[0].to, "sender@example.com");
        let sms_sent = sms_calls.lock().unwrap();
        assert_eq!(sms_sent[0].channel, channel::SMS);
        assert_eq!(sms_sent[0].to, "sms:fixture-sender");
    }

    fn social_envelope(
        channel_name: &str,
        visibility: &str,
        interaction: &str,
    ) -> OutboundEnvelope {
        OutboundEnvelope {
            channel: channel_name.into(),
            account: "page-reply".into(),
            interaction_type: interaction.into(),
            provider_post_id: Some("post-1".into()),
            reply_to_provider_message_id: Some("parent-1".into()),
            provider_thread_ref: None,
            to: "reader-1".into(),
            subject: None,
            body: "A public reply.".into(),
            purpose: store::inbox::purpose::MATTER.into(),
            visibility: Some(visibility.into()),
        }
    }

    #[tokio::test]
    async fn unconfigured_and_mismatched_channels_fail_closed() {
        assert_eq!(
            UnconfiguredSocial
                .deliver(&social_envelope(channel::FACEBOOK, "public", "comment"))
                .await
                .unwrap(),
            rejected("social provider transport is not configured")
        );
        assert_eq!(
            UnconfiguredSms
                .deliver(&envelope(channel::SMS, "body"))
                .await
                .unwrap(),
            TransportOutcome::Rejected {
                detail: "sms transport is not configured".into(),
            }
        );
        let email = Arc::new(ScriptedEmail {
            result: ScriptedSend::Submitted(SendReceipt { message_id: None }),
            sent: Mutex::new(Vec::new()),
        });
        let transport = ChannelTransport::new(email, Arc::new(UnconfiguredSms));
        assert!(transport
            .deliver(&envelope("carrier-pigeon", "body"))
            .await
            .is_err());
        let social = transport
            .deliver(&social_envelope(channel::INSTAGRAM, "public", "comment"))
            .await
            .unwrap();
        assert_eq!(
            social,
            rejected("social provider transport is not configured")
        );
    }

    #[test]
    fn classify_social_response_reads_message_id_and_generic_client_errors() {
        assert_eq!(
            classify_social_response(reqwest::StatusCode::OK, br#"{"message_id":"mid-9"}"#),
            TransportOutcome::Submitted {
                provider_ref: "mid-9".into(),
            }
        );
        assert_eq!(
            classify_social_response(reqwest::StatusCode::OK, b"not-json"),
            TransportOutcome::Unknown
        );
        assert_eq!(
            classify_social_response(reqwest::StatusCode::FORBIDDEN, b""),
            rejected("social provider rejected the reply (403 Forbidden)")
        );
    }

    #[test]
    fn a_tiktok_reply_needs_both_comment_grants() {
        let publishing = SocialProvider::Tiktok
            .required_scopes()
            .iter()
            .map(|scope| (*scope).to_string())
            .collect::<Vec<_>>();
        let with = |extra: &[&str]| {
            let mut scopes = publishing.clone();
            scopes.extend(extra.iter().map(|scope| (*scope).to_string()));
            scopes
        };
        for (granted, can_reply) in [
            // A publishing connection holds neither grant.
            (with(&[]), false),
            // Managing comments without reading them would skip the parent
            // comment's visibility check.
            (with(&["comment.list.manage"]), false),
            (with(&["comment.list"]), false),
            (with(&["comment.list", "comment.list.manage"]), true),
        ] {
            assert_eq!(tiktok_can_reply(&granted), can_reply, "{granted:?}");
        }
    }
}
