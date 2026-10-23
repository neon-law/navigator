//! Signed Twilio SMS intake and delivery callbacks for the shared Inbox.
//!
//! Each text enters the receiving Brand's Firm transcript. When configured,
//! the existing `EmailSummary` lane also retains its receipt and summary.
//! Attachments remain in Twilio; the summary records their count.
//!
//! Auth: every request carries `X-Twilio-Signature`, the base64 HMAC-SHA1 of
//! the webhook URL followed by each POST parameter's name and value in name
//! order, keyed by [`TWILIO_AUTH_TOKEN`]. Twilio signs the URL it was told to
//! call, so the handler rebuilds it from `NAVIGATOR_PUBLIC_HOST` (or the
//! deployed pod's `CANONICAL_HOST`) rather than
//! trusting the forwarded request line.
//!
//! The lane is off unless the deployment names a number. A named number
//! without the token or a public host fails boot rather than accepting
//! texts it cannot authenticate. Intake requires a Brand with an owning Firm.

use std::fmt::Write as _;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use thiserror::Error;

use crate::inbound_email::{InboundEmail, SmtpEnvelope};

/// The firm's SMS number in E.164 form, e.g. `+15108002090`. Unset or blank
/// keeps the lane off. Not a secret: it is printed on the firm's website.
pub const NAVIGATOR_SMS_NUMBER: &str = "NAVIGATOR_SMS_NUMBER";

/// Comma-separated E.164 numbers a deployment on simulated matters (persistent
/// staging) may still text. Unset or blank keeps every send refused there. Not
/// read by a production deployment, which has no such restriction. A secret:
/// the numbers are people's, so deployments keep them in `secrets.enc.yaml`.
pub const NAVIGATOR_SMS_TEST_RECIPIENTS: &str = "NAVIGATOR_SMS_TEST_RECIPIENTS";

/// The Twilio account's auth token, the HMAC key for `X-Twilio-Signature`.
/// A secret, delivered through the deployment Secret.
pub const TWILIO_AUTH_TOKEN: &str = "TWILIO_AUTH_TOKEN";

/// Where the deployment's Twilio number sends "A message comes in".
pub const SMS_WEBHOOK_PATH: &str = "/webhook/twilio/sms";
pub const SMS_STATUS_PATH: &str = "/webhook/twilio/status";

const SIGNATURE_HEADER: &str = "x-twilio-signature";

/// An empty TwiML document: acknowledge the text without auto-replying.
const EMPTY_TWIML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>";

/// A resolved SMS intake: the number texts must be addressed to, the key
/// that authenticates them, and the exact URL Twilio signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmsIntakeConfig {
    pub number: String,
    pub auth_token: String,
    pub webhook_url: String,
}

/// Why [`SmsIntakeConfig::from_lookup`] refused a partial configuration.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SmsConfigError {
    #[error("NAVIGATOR_SMS_NUMBER must be E.164, e.g. +15108002090")]
    InvalidNumber,
    #[error("NAVIGATOR_SMS_NUMBER is set but TWILIO_AUTH_TOKEN is not")]
    MissingAuthToken,
    #[error("NAVIGATOR_SMS_NUMBER is set but neither NAVIGATOR_PUBLIC_HOST nor CANONICAL_HOST is")]
    MissingPublicHost,
}

impl SmsIntakeConfig {
    /// Read the lane from the process environment.
    pub fn from_env() -> Result<Option<Self>, SmsConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Testable seam for [`Self::from_env`].
    pub fn from_lookup<F: Fn(&str) -> Option<String>>(
        get: F,
    ) -> Result<Option<Self>, SmsConfigError> {
        let non_empty = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(number) = non_empty(NAVIGATOR_SMS_NUMBER) else {
            return Ok(None);
        };
        if !is_e164(&number) {
            return Err(SmsConfigError::InvalidNumber);
        }
        let auth_token = non_empty(TWILIO_AUTH_TOKEN).ok_or(SmsConfigError::MissingAuthToken)?;
        // A GKE pod gets the host as `CANONICAL_HOST`: `ops ship` rewrites every
        // literal `NAVIGATOR_PUBLIC_HOST` in the manifest, env names included.
        let host = non_empty("NAVIGATOR_PUBLIC_HOST")
            .or_else(|| non_empty("CANONICAL_HOST"))
            .ok_or(SmsConfigError::MissingPublicHost)?;
        let host = host
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/');
        Ok(Some(Self {
            number,
            auth_token,
            webhook_url: format!("https://{host}{SMS_WEBHOOK_PATH}"),
        }))
    }

    /// The receipt's receiving mailbox for a text to this number.
    #[must_use]
    pub fn mailbox(&self) -> String {
        format!("sms:{}", self.number)
    }
}

fn is_e164(number: &str) -> bool {
    number.strip_prefix('+').is_some_and(|digits| {
        (8..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Twilio's signature: HMAC-SHA1 over the URL, then every parameter's name
/// and value concatenated in name order, base64-encoded.
#[must_use]
pub fn twilio_signature(auth_token: &str, url: &str, params: &[(String, String)]) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(signing_mac(auth_token, url, params).finalize().into_bytes())
}

fn signing_mac(auth_token: &str, url: &str, params: &[(String, String)]) -> Hmac<Sha1> {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort();
    let mut mac =
        Hmac::<Sha1>::new_from_slice(auth_token.as_bytes()).expect("HMAC accepts any key length");
    mac.update(url.as_bytes());
    for (name, value) in sorted {
        mac.update(name.as_bytes());
        mac.update(value.as_bytes());
    }
    mac
}

/// Constant-time check of a provided `X-Twilio-Signature`.
fn signature_matches(
    config: &SmsIntakeConfig,
    params: &[(String, String)],
    provided: &str,
) -> bool {
    let Ok(provided) = base64::engine::general_purpose::STANDARD.decode(provided.trim()) else {
        return false;
    };
    signing_mac(&config.auth_token, &config.webhook_url, params)
        .verify_slice(&provided)
        .is_ok()
}

/// One inbound text, reduced to the fields the summary lane reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundSms {
    pub message_sid: String,
    pub from: String,
    pub to: String,
    pub body: String,
    pub media_count: usize,
}

impl InboundSms {
    fn from_params(params: &[(String, String)]) -> Option<Self> {
        let field = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        Some(Self {
            message_sid: field("MessageSid").filter(|v| !v.is_empty())?,
            from: field("From").filter(|v| !v.is_empty())?,
            to: field("To").filter(|v| !v.is_empty())?,
            body: field("Body").unwrap_or_default(),
            media_count: field("NumMedia").and_then(|v| v.parse().ok()).unwrap_or(0),
        })
    }

    /// The text as a deterministic `text/plain` message. No clock value
    /// enters the bytes, so a redelivered `MessageSid` digests to the same
    /// receipt and the same workflow run.
    #[must_use]
    pub fn to_rfc5322(&self) -> Vec<u8> {
        let header = |value: &str| value.replace(['\r', '\n'], " ");
        let (from, to, sid) = (
            header(&self.from),
            header(&self.to),
            header(&self.message_sid),
        );
        let mut body = format!("Text message from {from} to {to}.\r\n\r\n");
        if self.body.trim().is_empty() {
            body.push_str("(no text)\r\n");
        } else {
            body.push_str(&self.body.replace("\r\n", "\n").replace('\n', "\r\n"));
            body.push_str("\r\n");
        }
        if self.media_count > 0 {
            let _ = write!(
                body,
                "\r\n({} picture or file attachment(s) are in the Twilio console, not retrieved.)\r\n",
                self.media_count
            );
        }
        format!(
            "From: {from} <sms@twilio.invalid>\r\n\
             To: {to} <sms@twilio.invalid>\r\n\
             Subject: Text message from {from}\r\n\
             Message-ID: <{sid}@sms.twilio.com>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: 8bit\r\n\
             \r\n\
             {body}"
        )
        .into_bytes()
    }

    fn into_email(self, mailbox: String) -> InboundEmail {
        let raw = self.to_rfc5322();
        InboundEmail {
            from: self.from.clone(),
            to: self.to.clone(),
            envelope: Some(SmtpEnvelope {
                to: vec![mailbox],
                from: Some(self.from.clone()),
            }),
            subject: format!("Text message from {}", self.from),
            text: self.body,
            raw,
            message_id: Some(format!("{}@sms.twilio.com", self.message_sid)),
            ..InboundEmail::default()
        }
    }
}

/// `POST /webhook/twilio/sms`
pub async fn webhook(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(config) = state.sms_intake.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let params: Vec<(String, String)> = url::form_urlencoded::parse(&body).into_owned().collect();
    let Some(provided) = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !signature_matches(config, &params, provided) {
        tracing::warn!("twilio sms webhook: signature mismatch");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(sms) = InboundSms::from_params(&params) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if sms.to != config.number {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(saved) = ingest_sms(&state.surreal, config, &sms).await else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if crate::inbox::start_inbound_message_alert(saved.message_id)
        .await
        .is_err()
    {
        tracing::error!("SMS notice submission failed; Message retained for reconciliation");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let Some(summary) = state.summary_intake.as_ref() else {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
            EMPTY_TWIML,
        )
            .into_response();
    };
    let Ok((receiving_firm, _)) = receiving_firm(&state.surreal, config).await else {
        tracing::error!("SMS receiving Firm lookup failed");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if receiving_firm != summary.firm_id {
        tracing::warn!("SMS summary configuration belongs to another Firm; Inbox receipt retained");
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
            EMPTY_TWIML,
        )
            .into_response();
    }
    tracing::info!(media = sms.media_count, "twilio sms received");
    let email = sms.into_email(config.mailbox());
    let receipt = match crate::inbound_email::persist_summary_receipt(
        &state.surreal,
        &state.storage,
        &summary.deployment,
        config.mailbox(),
        &email,
        Some(summary.firm_id),
        None,
        false,
    )
    .await
    {
        Ok(receipt) => receipt,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = crate::inbound_email::start_summary_workflow(summary, &receipt).await {
        return error.into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        EMPTY_TWIML,
    )
        .into_response()
}

pub(crate) async fn receiving_firm(
    db: &store::surreal::SurrealDb,
    config: &SmsIntakeConfig,
) -> Result<(uuid::Uuid, views::brand::BrandKey), String> {
    let url = reqwest::Url::parse(&config.webhook_url).map_err(|_| "invalid SMS webhook URL")?;
    let brand = url
        .host_str()
        .and_then(views::brand::registered_brand_key)
        .ok_or("SMS receiving number has no registered Brand")?;
    let firm = store::firms::firm_id_for_brand_key(db, brand.as_str())
        .await
        .map_err(|_| "SMS Brand lookup failed")?
        .ok_or("SMS Brand has no owning Firm")?;
    Ok((firm, brand))
}

async fn ingest_sms(
    db: &store::surreal::SurrealDb,
    config: &SmsIntakeConfig,
    sms: &InboundSms,
) -> Result<store::inbox::IngestedInbound, String> {
    let (firm_id, brand) = receiving_firm(db, config).await?;
    let body = if sms.media_count == 0 {
        sms.body.clone()
    } else {
        format!(
            "{}\n\n{} picture or file attachment(s) remain in Twilio.",
            sms.body, sms.media_count
        )
    };
    store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            firm_id,
            brand_key: Some(brand.as_str()),
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: store::inbox::channel::SMS,
            provider_account: &config.number,
            provider_message_id: &sms.message_sid,
            provider_thread_ref: None,
            external_address: &sms.from,
            external_name: None,
            subject: None,
            body: &body,
            source_page: Some(&config.number),
            source_channel: Some(store::inbox::channel::SMS),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .map_err(|error| error.to_string())
}

/// Signed Twilio callbacks update the same outbound Message as the reply.
pub async fn status(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(config) = state.sms_intake.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let params: Vec<(String, String)> = url::form_urlencoded::parse(&body).into_owned().collect();
    let mut signed_config = config.clone();
    signed_config.webhook_url = config
        .webhook_url
        .replace(SMS_WEBHOOK_PATH, SMS_STATUS_PATH);
    let signature = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !signature_matches(&signed_config, &params, signature) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let field = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if field("From").is_some_and(|from| from != config.number) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(provider_ref) = field("MessageSid") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let delivery = match field("MessageStatus") {
        Some("delivered" | "read") => store::inbox::delivery::DELIVERED,
        Some("failed" | "undelivered" | "canceled") => store::inbox::delivery::FAILED,
        Some("queued" | "accepted" | "sending" | "sent") => store::inbox::delivery::SUBMITTED,
        _ => return StatusCode::NO_CONTENT.into_response(),
    };
    let Ok((firm_id, _)) = receiving_firm(&state.surreal, config).await else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if store::inbox::apply_delivery(
        &state.surreal,
        firm_id,
        store::inbox::channel::SMS,
        &config.number,
        provider_ref,
        delivery,
    )
    .await
    .is_ok()
    {
        StatusCode::NO_CONTENT.into_response()
    } else {
        tracing::error!("SMS delivery update failed");
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    fn config() -> SmsIntakeConfig {
        SmsIntakeConfig::from_lookup(lookup(&[
            (NAVIGATOR_SMS_NUMBER, "+15108002090"),
            (TWILIO_AUTH_TOKEN, "12345"),
            ("NAVIGATOR_PUBLIC_HOST", "www.example.com"),
        ]))
        .unwrap()
        .unwrap()
    }

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn an_unset_number_keeps_the_lane_off() {
        assert_eq!(SmsIntakeConfig::from_lookup(lookup(&[])), Ok(None));
        assert_eq!(
            SmsIntakeConfig::from_lookup(lookup(&[(NAVIGATOR_SMS_NUMBER, "  ")])),
            Ok(None)
        );
    }

    #[test]
    fn a_named_number_without_its_coordinates_fails_closed() {
        assert_eq!(
            SmsIntakeConfig::from_lookup(lookup(&[(NAVIGATOR_SMS_NUMBER, "510-800-2090")])),
            Err(SmsConfigError::InvalidNumber)
        );
        assert_eq!(
            SmsIntakeConfig::from_lookup(lookup(&[(NAVIGATOR_SMS_NUMBER, "+15108002090")])),
            Err(SmsConfigError::MissingAuthToken)
        );
        assert_eq!(
            SmsIntakeConfig::from_lookup(lookup(&[
                (NAVIGATOR_SMS_NUMBER, "+15108002090"),
                (TWILIO_AUTH_TOKEN, "12345"),
            ])),
            Err(SmsConfigError::MissingPublicHost)
        );
    }

    #[test]
    fn the_signed_url_is_the_public_host_webhook() {
        assert_eq!(
            config().webhook_url,
            "https://www.example.com/webhook/twilio/sms"
        );
        assert_eq!(config().mailbox(), "sms:+15108002090");
    }

    /// A GKE pod receives the row's public host as `CANONICAL_HOST` only:
    /// `ops ship` rewrites every literal `NAVIGATOR_PUBLIC_HOST` in the
    /// manifest to the host, so no env entry can carry that name. 26.9.30
    /// crash-looped on staging until this fallback existed.
    #[test]
    fn the_deployed_canonical_host_signs_the_webhook() {
        let config = SmsIntakeConfig::from_lookup(lookup(&[
            (NAVIGATOR_SMS_NUMBER, "+15108002090"),
            (TWILIO_AUTH_TOKEN, "12345"),
            ("CANONICAL_HOST", "staging.example.com"),
        ]))
        .expect("CANONICAL_HOST satisfies the public host")
        .expect("a named number turns the lane on");
        assert_eq!(
            config.webhook_url,
            "https://staging.example.com/webhook/twilio/sms"
        );
    }

    /// Pinned against an independent HMAC-SHA1 (Python's `hmac`) over the
    /// same URL and name-sorted parameters, so a reordering or a dropped
    /// separator in [`twilio_signature`] cannot pass by agreeing with itself.
    #[test]
    fn signature_matches_twilio_algorithm() {
        let signed = params(&[
            ("To", "+15108002090"),
            ("From", "+15555550100"),
            ("Body", "Hello"),
            ("MessageSid", "SM123"),
        ]);
        assert_eq!(
            twilio_signature(
                "12345",
                "https://www.example.com/webhook/twilio/sms",
                &signed
            ),
            "vSMrFg0ktZZDl1G5odfhAhWJAiQ="
        );
    }

    #[test]
    fn a_tampered_body_fails_the_signature() {
        let config = config();
        let signed = params(&[("Body", "Hello"), ("MessageSid", "SM123")]);
        let signature = twilio_signature(&config.auth_token, &config.webhook_url, &signed);
        assert!(signature_matches(&config, &signed, &signature));
        let tampered = params(&[("Body", "Hullo"), ("MessageSid", "SM123")]);
        assert!(!signature_matches(&config, &tampered, &signature));
        assert!(!signature_matches(&config, &signed, "not base64!"));
    }

    #[test]
    fn a_text_becomes_a_summarizable_message() {
        let sms = InboundSms::from_params(&params(&[
            ("MessageSid", "SM123"),
            ("From", "+15555550100"),
            ("To", "+15108002090"),
            ("Body", "Can I reschedule?\nThanks"),
            ("NumMedia", "1"),
        ]))
        .unwrap();
        let raw = sms.to_rfc5322();
        assert_eq!(raw, sms.to_rfc5322(), "the bytes are deterministic");
        let normalized = workflows::normalize_email(&raw, 32_000).unwrap();
        assert!(normalized
            .body
            .starts_with("Text message from +15555550100 to +15108002090."));
        assert!(
            normalized.body.contains("Can I reschedule?"),
            "{}",
            normalized.body
        );
        assert!(normalized.body.contains("Thanks"), "{}", normalized.body);
        assert!(normalized.body.contains("1 picture or file attachment(s)"));
        assert_eq!(
            crate::inbound_email::message_id_from_raw(&raw).as_deref(),
            Some("SM123@sms.twilio.com")
        );
    }

    #[test]
    fn a_header_value_cannot_inject_a_header() {
        let sms = InboundSms {
            message_sid: "SM1\r\nBcc: x@example.com".into(),
            from: "+15555550100".into(),
            to: "+15108002090".into(),
            body: "hi".into(),
            media_count: 0,
        };
        let raw = String::from_utf8(sms.to_rfc5322()).unwrap();
        assert!(!raw.contains("\r\nBcc:"), "{raw}");
    }

    #[test]
    fn missing_sid_or_sender_is_rejected() {
        assert!(InboundSms::from_params(&params(&[("From", "+1"), ("To", "+2")])).is_none());
        assert!(InboundSms::from_params(&params(&[("MessageSid", "SM1"), ("To", "+2")])).is_none());
    }

    #[tokio::test]
    async fn signed_sms_is_saved_without_a_summary_service_and_unsigned_callbacks_are_denied() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let mut state = crate::test_support::app_state(db.clone()).await;
        let mut config = config();
        config.webhook_url = "https://www.neonlaw.com/webhook/twilio/sms".into();
        state.sms_intake = Some(config.clone());
        state.summary_intake = None;
        let fields = params(&[
            ("MessageSid", "synthetic-sid"),
            ("From", "+15555550100"),
            ("To", &config.number),
            ("Body", "Synthetic inquiry"),
        ]);
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            twilio_signature(&config.auth_token, &config.webhook_url, &fields)
                .parse()
                .unwrap(),
        );
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&fields)
            .finish();
        let response = webhook(State(state.clone()), headers, Bytes::from(body)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            store::inbox_alerts::awaiting_reconciliation(&db, 15)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            status(
                State(state),
                HeaderMap::new(),
                Bytes::from_static(b"MessageSid=synthetic-sid&MessageStatus=delivered")
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn signed_delivery_callback_updates_the_original_outbound_message() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let mut config = config();
        config.webhook_url = "https://www.neonlaw.com/webhook/twilio/sms".into();
        let sms = InboundSms {
            message_sid: "synthetic-inbound".into(),
            from: "+15555550100".into(),
            to: config.number.clone(),
            body: "Synthetic inquiry".into(),
            media_count: 0,
        };
        let saved = ingest_sms(&db, &config, &sms).await.unwrap();
        db.query("UPDATE $id SET direction = 'outbound', provider_ref = 'synthetic-outbound', delivery_state = 'submitted'")
            .bind(("id", store::surreal::record_id("message", saved.message_id))).await.unwrap().check().unwrap();
        let mut state = crate::test_support::app_state(db.clone()).await;
        state.sms_intake = Some(config.clone());
        let fields = params(&[
            ("MessageSid", "synthetic-outbound"),
            ("MessageStatus", "delivered"),
            ("From", &config.number),
        ]);
        let callback = config
            .webhook_url
            .replace(SMS_WEBHOOK_PATH, SMS_STATUS_PATH);
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            twilio_signature(&config.auth_token, &callback, &fields)
                .parse()
                .unwrap(),
        );
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(&fields)
            .finish();
        assert_eq!(
            status(State(state), headers, Bytes::from(body))
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            store::inbox::message_by_id(&db, saved.message_id)
                .await
                .unwrap()
                .unwrap()
                .delivery_state,
            "delivered"
        );
    }

    #[tokio::test]
    async fn a_picture_only_message_keeps_a_transcript_receipt() {
        let db = store::test_support::mem_surreal().await;
        store::test_support::seed_firm_with_brand(&db, "neon").await;
        let mut config = config();
        config.webhook_url = "https://www.neonlaw.com/webhook/twilio/sms".into();
        let sms = InboundSms {
            message_sid: "synthetic-media-id".into(),
            from: "synthetic-sender".into(),
            to: config.number.clone(),
            body: String::new(),
            media_count: 1,
        };
        let saved = ingest_sms(&db, &config, &sms).await.unwrap();
        let message = store::inbox::message_by_id(&db, saved.message_id)
            .await
            .unwrap()
            .unwrap();
        assert!(message
            .body
            .contains("1 picture or file attachment(s) remain in Twilio"));
        assert!(message.attachment_asset_id.is_none());
    }

    #[tokio::test]
    async fn an_unconfigured_deployment_does_not_expose_the_route() {
        let state = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let response = webhook(State(state), HeaderMap::new(), Bytes::new()).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unsigned_text_is_refused() {
        let mut state =
            crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        state.sms_intake = Some(config());
        let body = Bytes::from_static(b"MessageSid=SM1&From=%2B15555550100&To=%2B15108002090");
        let response = webhook(State(state.clone()), HeaderMap::new(), body.clone()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let mut headers = HeaderMap::new();
        headers.insert(SIGNATURE_HEADER, "AAAA".parse().unwrap());
        let response = webhook(State(state), headers, body).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn a_signed_text_to_another_number_is_not_ours() {
        let mut state =
            crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let config = config();
        state.sms_intake = Some(config.clone());
        let signed = params(&[
            ("MessageSid", "SM1"),
            ("From", "+15555550100"),
            ("To", "+15550000000"),
        ]);
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            twilio_signature(&config.auth_token, &config.webhook_url, &signed)
                .parse()
                .unwrap(),
        );
        let body = Bytes::from_static(b"MessageSid=SM1&From=%2B15555550100&To=%2B15550000000");
        let response = webhook(State(state), headers, body).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn twilio_intake_saves_one_shared_message_even_without_summaries() {
        let db = store::test_support::mem_surreal().await;
        let firm = store::test_support::seed_firm_with_brand(&db, "neon").await;
        let mut receiving = config();
        receiving.webhook_url = "https://www.neonlaw.com/webhook/twilio/sms".into();
        let sms = InboundSms {
            message_sid: "synthetic-sms-id".into(),
            from: "synthetic-sender".into(),
            to: receiving.number.clone(),
            body: "Synthetic inquiry".into(),
            media_count: 0,
        };
        let first = ingest_sms(&db, &receiving, &sms).await.unwrap();
        let replay = ingest_sms(&db, &receiving, &sms).await.unwrap();
        assert_eq!(first.message_id, replay.message_id);
        assert!(replay.deduped);
        let conversation = store::inbox::conversation_by_id(&db, first.conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conversation.firm_id, firm);
        assert_eq!(conversation.channel, store::inbox::channel::SMS);
        assert_eq!(conversation.provider_account, receiving.number);
    }
}
