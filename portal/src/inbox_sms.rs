//! Twilio replies for the shared Message transcript.

use async_trait::async_trait;
use store::config::{SocialEgressDenied, SocialEgressGate};
use store::inbox::{channel, OutboundEnvelope, OutboundTransport, TransportOutcome};

use crate::twilio_sms::{
    SmsIntakeConfig, NAVIGATOR_SMS_TEST_RECIPIENTS, SMS_STATUS_PATH, SMS_WEBHOOK_PATH,
};

fn parse_recipients(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|number| !number.is_empty())
        .map(str::to_string)
        .collect()
}

/// Dispatches only through the configured receiving number and account.
pub struct TwilioSmsTransport {
    config: Option<SmsIntakeConfig>,
    account_sid: Option<String>,
    egress: SocialEgressGate,
    test_recipients: Vec<String>,
    http: reqwest::Client,
    api_base: String,
}

impl TwilioSmsTransport {
    #[must_use]
    pub fn new(config: Option<SmsIntakeConfig>) -> Self {
        Self {
            config,
            account_sid: std::env::var("TWILIO_ACCOUNT_SID")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            egress: SocialEgressGate::from_process_env(),
            test_recipients: std::env::var(NAVIGATOR_SMS_TEST_RECIPIENTS)
                .map(|value| parse_recipients(&value))
                .unwrap_or_default(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            api_base: "https://api.twilio.com/2010-04-01".into(),
        }
    }
}

#[async_trait]
impl OutboundTransport for TwilioSmsTransport {
    async fn deliver(&self, envelope: &OutboundEnvelope) -> Result<TransportOutcome, String> {
        let refused = |detail: &str| TransportOutcome::Rejected {
            detail: detail.into(),
        };
        if envelope.channel != channel::SMS {
            return Ok(refused("Twilio accepts SMS replies only"));
        }
        // Staging runs the production profile over simulated matters, so the
        // gate refuses it; a text to an allowlisted test number is the one
        // send it may make. Any other denial, or any other number, stays refused.
        match self.egress.check() {
            Ok(()) => {}
            Err(SocialEgressDenied::SimulatedMatters)
                if self.test_recipients.contains(&envelope.to) => {}
            Err(_) => {
                return Ok(refused(
                    "SMS sending is disabled outside a live production deployment",
                ));
            }
        }
        let (Some(config), Some(account_sid)) = (&self.config, &self.account_sid) else {
            return Ok(refused(
                "SMS replies require a receiving number, TWILIO_AUTH_TOKEN and TWILIO_ACCOUNT_SID",
            ));
        };
        if envelope.account != config.number {
            return Ok(refused(
                "SMS conversation belongs to another receiving account",
            ));
        }
        let callback = config
            .webhook_url
            .replace(SMS_WEBHOOK_PATH, SMS_STATUS_PATH);
        let response = self
            .http
            .post(format!(
                "{}/Accounts/{account_sid}/Messages.json",
                self.api_base
            ))
            .basic_auth(account_sid, Some(&config.auth_token))
            .form(&[
                ("From", config.number.as_str()),
                ("To", envelope.to.as_str()),
                ("Body", envelope.body.as_str()),
                ("StatusCallback", callback.as_str()),
            ])
            .send()
            .await;
        let Ok(response) = response else {
            return Ok(TransportOutcome::Unknown);
        };
        if response.status().is_client_error() {
            return Ok(refused(
                "Twilio rejected the SMS reply; check the configured account and number",
            ));
        }
        if !response.status().is_success() {
            return Ok(TransportOutcome::Unknown);
        }
        let Ok(body) = response.json::<serde_json::Value>().await else {
            return Ok(TransportOutcome::Unknown);
        };
        Ok(body
            .get("sid")
            .and_then(serde_json::Value::as_str)
            .filter(|sid| !sid.is_empty())
            .map_or(TransportOutcome::Unknown, |sid| {
                TransportOutcome::Submitted {
                    provider_ref: sid.into(),
                }
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> OutboundEnvelope {
        OutboundEnvelope {
            channel: channel::SMS.into(),
            account: "receiving-account".into(),
            interaction_type: "message".into(),
            provider_post_id: None,
            reply_to_provider_message_id: None,
            provider_thread_ref: None,
            to: "synthetic-recipient".into(),
            subject: None,
            body: "Synthetic reply".into(),
            purpose: "matter".into(),
            visibility: None,
        }
    }

    #[tokio::test]
    async fn twilio_reply_retains_the_receiving_account_and_provider_receipt() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Accounts/synthetic-account/Messages.json"))
            .and(body_string_contains("From=receiving-account"))
            .and(body_string_contains("To=synthetic-recipient"))
            .and(body_string_contains(
                "StatusCallback=https%3A%2F%2Fwww.neonlaw.com%2Fwebhook%2Ftwilio%2Fstatus",
            ))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"sid":"synthetic-message"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut transport = TwilioSmsTransport::new(Some(SmsIntakeConfig {
            number: "receiving-account".into(),
            auth_token: "synthetic-token".into(),
            webhook_url: "https://www.neonlaw.com/webhook/twilio/sms".into(),
        }));
        transport.account_sid = Some("synthetic-account".into());
        transport.egress = SocialEgressGate::new(|| Ok(()));
        transport.api_base = server.uri();
        assert!(matches!(transport.deliver(&envelope()).await.unwrap(),
            TransportOutcome::Submitted { provider_ref } if provider_ref == "synthetic-message"));
        let mut foreign = envelope();
        foreign.account = "another-account".into();
        assert!(matches!(
            transport.deliver(&foreign).await.unwrap(),
            TransportOutcome::Rejected { .. }
        ));
    }

    #[tokio::test]
    async fn simulated_staging_texts_only_its_allowlisted_test_numbers() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Accounts/synthetic-account/Messages.json"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"sid":"staging-sid"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut transport = TwilioSmsTransport::new(Some(SmsIntakeConfig {
            number: "receiving-account".into(),
            auth_token: "synthetic-token".into(),
            webhook_url: "https://staging.neonlaw.com/webhook/twilio/sms".into(),
        }));
        transport.account_sid = Some("synthetic-account".into());
        transport.api_base = server.uri();
        transport.test_recipients = parse_recipients(" synthetic-recipient , ,+15550100999");
        assert_eq!(
            transport.test_recipients,
            vec![
                "synthetic-recipient".to_string(),
                "+15550100999".to_string()
            ]
        );

        transport.egress =
            SocialEgressGate::new(|| Err(store::config::SocialEgressDenied::SimulatedMatters));
        assert!(matches!(
            transport.deliver(&envelope()).await.unwrap(),
            TransportOutcome::Submitted { provider_ref } if provider_ref == "staging-sid"
        ));
        let mut stranger = envelope();
        stranger.to = "not-on-the-list".into();
        assert!(matches!(
            transport.deliver(&stranger).await.unwrap(),
            TransportOutcome::Rejected { .. }
        ));

        // The allowlist loosens only the simulated-matters denial.
        transport.egress =
            SocialEgressGate::new(|| Err(store::config::SocialEgressDenied::NotProduction));
        assert!(matches!(
            transport.deliver(&envelope()).await.unwrap(),
            TransportOutcome::Rejected { .. }
        ));
    }

    #[tokio::test]
    async fn a_disabled_deployment_never_calls_twilio() {
        let mut transport = TwilioSmsTransport::new(None);
        transport.egress =
            SocialEgressGate::new(|| Err(store::config::SocialEgressDenied::NotProduction));
        transport.api_base = "http://127.0.0.1:1".into();
        assert!(matches!(
            transport.deliver(&envelope()).await.unwrap(),
            TransportOutcome::Rejected { .. }
        ));
    }

    #[tokio::test]
    async fn a_missing_account_is_an_actionable_rejection() {
        let mut transport = TwilioSmsTransport::new(None);
        transport.egress = SocialEgressGate::new(|| Ok(()));
        let TransportOutcome::Rejected { detail } = transport.deliver(&envelope()).await.unwrap()
        else {
            panic!("missing account must reject")
        };
        assert!(detail.contains("TWILIO_ACCOUNT_SID"));
    }
}
