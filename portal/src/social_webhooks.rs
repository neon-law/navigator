//! Verified Meta social webhooks projected into the shared Message transcript.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{body::Bytes, Router};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::Value;
use sha2::Sha256;

const SIGNATURE_HEADER: &str = "x-hub-signature-256";
const MAX_WEBHOOK_BYTES: usize = 2 * 1024 * 1024;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetaInteraction {
    provider: store::social_accounts::SocialProvider,
    account_id: String,
    message_id: String,
    thread_ref: Option<String>,
    post_id: Option<String>,
    parent_id: Option<String>,
    interaction_type: &'static str,
    visibility: Option<&'static str>,
    external_id: String,
    external_name: Option<String>,
    body: String,
    occurred_at: String,
}

#[derive(Deserialize)]
struct ChallengeQuery {
    #[serde(rename = "hub.mode")]
    mode: String,
    #[serde(rename = "hub.verify_token")]
    verify_token: String,
    #[serde(rename = "hub.challenge")]
    challenge: String,
}

#[derive(Deserialize)]
struct WebhookPayload {
    object: String,
    #[serde(default)]
    entry: Vec<WebhookEntry>,
}

#[derive(Deserialize)]
struct WebhookEntry {
    id: String,
    #[serde(default)]
    changes: Vec<WebhookChange>,
    #[serde(default)]
    messaging: Vec<Value>,
}

#[derive(Deserialize)]
struct WebhookChange {
    field: String,
    value: Value,
}

fn provider_for_object(object: &str) -> Option<store::social_accounts::SocialProvider> {
    use store::social_accounts::SocialProvider;
    match object {
        "page" => Some(SocialProvider::Facebook),
        "instagram" => Some(SocialProvider::Instagram),
        "threads" => Some(SocialProvider::Threads),
        _ => None,
    }
}

fn occurred_at(value: Option<&Value>) -> String {
    value
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .or_else(|| {
            value
                .and_then(Value::as_str)
                .and_then(|date| DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.with_timezone(&Utc))
        })
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn parse_meta_payload(body: &[u8]) -> Result<Vec<MetaInteraction>, ()> {
    let payload: WebhookPayload = serde_json::from_slice(body).map_err(|_| ())?;
    let provider = provider_for_object(&payload.object).ok_or(())?;
    let mut interactions = Vec::new();
    for entry in payload.entry {
        if entry.id.trim().is_empty() {
            return Err(());
        }
        for change in entry.changes {
            if change.field != "comments" {
                continue;
            }
            let value = change.value;
            if value
                .get("verb")
                .and_then(Value::as_str)
                .is_some_and(|verb| verb != "add")
            {
                continue;
            }
            let Some(message_id) = value.get("id").and_then(Value::as_str) else {
                return Err(());
            };
            let Some(body_text) = value.get("text").and_then(Value::as_str) else {
                continue;
            };
            let post_id = value
                .get("media")
                .and_then(|media| media.get("id"))
                .and_then(Value::as_str)
                .or_else(|| value.get("post_id").and_then(Value::as_str))
                .map(str::to_string);
            let external = value.get("from").unwrap_or(&Value::Null);
            let external_id = external
                .get("id")
                .and_then(Value::as_str)
                .or_else(|| external.get("username").and_then(Value::as_str))
                .unwrap_or("unknown-sender")
                .to_string();
            interactions.push(MetaInteraction {
                provider,
                account_id: entry.id.clone(),
                message_id: message_id.to_string(),
                thread_ref: post_id.clone(),
                post_id,
                parent_id: value
                    .get("parent_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                interaction_type: "comment",
                visibility: Some("public"),
                external_id,
                external_name: external
                    .get("username")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                body: body_text.to_string(),
                occurred_at: occurred_at(
                    value.get("timestamp").or_else(|| value.get("created_time")),
                ),
            });
        }
        for event in entry.messaging {
            let message = event.get("message").unwrap_or(&Value::Null);
            if message.get("is_echo").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            let Some(message_id) = message.get("mid").and_then(Value::as_str) else {
                continue;
            };
            let Some(body_text) = message.get("text").and_then(Value::as_str) else {
                continue;
            };
            let Some(sender_id) = event
                .get("sender")
                .and_then(|sender| sender.get("id"))
                .and_then(Value::as_str)
            else {
                return Err(());
            };
            let timestamp = event
                .get("timestamp")
                .and_then(Value::as_i64)
                .map(|ms| Value::from(ms / 1000));
            interactions.push(MetaInteraction {
                provider,
                account_id: entry.id.clone(),
                message_id: message_id.to_string(),
                thread_ref: Some(sender_id.to_string()),
                post_id: None,
                parent_id: None,
                interaction_type: "direct_message",
                visibility: Some("private"),
                external_id: sender_id.to_string(),
                external_name: None,
                body: body_text.to_string(),
                occurred_at: occurred_at(timestamp.as_ref()),
            });
        }
    }
    Ok(interactions)
}

fn verify_signature(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    let Some(value) = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(hex) = value.strip_prefix("sha256=") else {
        return false;
    };
    if hex.len() != 64 {
        return false;
    }
    let mut signature = [0_u8; 32];
    for (index, byte) in signature.iter_mut().enumerate() {
        let offset = index * 2;
        let Ok(value) = u8::from_str_radix(&hex[offset..offset + 2], 16) else {
            return false;
        };
        *byte = value;
    }
    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// `GET /webhook/meta` subscription challenge.
async fn challenge(Query(query): Query<ChallengeQuery>) -> Response {
    let Ok(expected) = std::env::var("NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if expected.is_empty()
        || query.mode != "subscribe"
        || !constant_time_eq(&expected, &query.verify_token)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    (StatusCode::OK, query.challenge).into_response()
}

/// `POST /webhook/meta` verified Meta events. Only text comments and text
/// direct messages are admitted; unsupported event classes are skipped.
async fn receive(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    if body.len() > MAX_WEBHOOK_BYTES {
        return StatusCode::PAYLOAD_TOO_LARGE;
    }
    let Ok(secret) = std::env::var("NAVIGATOR_META_WEBHOOK_APP_SECRET") else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    if secret.is_empty() || !verify_signature(&secret, &headers, &body) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(interactions) = parse_meta_payload(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    for interaction in interactions {
        let Ok(accounts) = store::social_accounts::for_provider_identity(
            &state.surreal,
            interaction.provider,
            &interaction.account_id,
        )
        .await
        else {
            return StatusCode::INTERNAL_SERVER_ERROR;
        };
        if accounts.len() != 1 {
            return StatusCode::UNPROCESSABLE_ENTITY;
        }
        let account = &accounts[0];
        let received = store::inbox::ingest_inbound(
            &state.surreal,
            &store::inbox::IngestInbound {
                firm_id: account.firm_id,
                brand_key: Some(&account.brand_key),
                interaction_type: Some(interaction.interaction_type),
                provider_post_id: interaction.post_id.as_deref(),
                parent_provider_message_id: interaction.parent_id.as_deref(),
                visibility: interaction.visibility,
                channel: interaction.provider.as_str(),
                provider_account: &interaction.account_id,
                provider_message_id: &interaction.message_id,
                provider_thread_ref: interaction.thread_ref.as_deref(),
                external_address: &interaction.external_id,
                external_name: interaction.external_name.as_deref(),
                subject: None,
                body: &interaction.body,
                source_page: Some(&interaction.account_id),
                source_channel: Some(interaction.provider.as_str()),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachment_asset_id: None,
                occurred_at: &interaction.occurred_at,
                raw_storage_key: None,
            },
        )
        .await;
        let Ok(received) = received else {
            return StatusCode::INTERNAL_SERVER_ERROR;
        };
        if start_alert(received.message_id).await.is_err() {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    StatusCode::ACCEPTED
}

async fn start_alert(message_id: uuid::Uuid) -> Result<(), String> {
    crate::inbox::start_inbound_message_alert(message_id).await
}

fn constant_time_eq(expected: &str, actual: &str) -> bool {
    let expected = expected.as_bytes();
    let actual = actual.as_bytes();
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

pub fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/webhook/meta", get(challenge).post(receive))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_WEBHOOK_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn signature_authenticates_exact_raw_body() {
        let body = br#"{"object":"instagram","entry":[]}"#;
        let secret = "synthetic-meta-secret";
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = mac.finalize().into_bytes();
        #[allow(clippy::format_collect)]
        let signature_hex = signature
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(&format!("sha256={signature_hex}")).unwrap(),
        );
        assert!(verify_signature(secret, &headers, body));
        assert!(!verify_signature(
            secret,
            &headers,
            br#"{"object":"instagram","entry":[]} "#
        ));
        assert!(!verify_signature("wrong-secret", &headers, body));
    }

    #[test]
    fn parses_comments_and_text_messages_with_brand_context_fields() {
        let body = br#"{"object":"instagram","entry":[{"id":"ig-account","changes":[{"field":"comments","value":{"id":"comment-1","text":"A question","timestamp":1791392400,"media":{"id":"post-1"},"from":{"id":"person-1","username":"reader"}}}],"messaging":[{"sender":{"id":"person-2"},"timestamp":1791392400000,"message":{"mid":"dm-1","text":"A private question"}}]}]}"#;
        let interactions = parse_meta_payload(body).unwrap();
        assert_eq!(interactions.len(), 2);
        assert_eq!(
            interactions[0].provider,
            store::social_accounts::SocialProvider::Instagram
        );
        assert_eq!(interactions[0].post_id.as_deref(), Some("post-1"));
        assert_eq!(interactions[0].visibility, Some("public"));
        assert_eq!(interactions[1].interaction_type, "direct_message");
        assert_eq!(interactions[1].visibility, Some("private"));
    }

    #[test]
    fn unsupported_objects_fail_closed_and_provider_echoes_are_ignored() {
        assert!(parse_meta_payload(br#"{"object":"unknown","entry":[]}"#).is_err());
        let echo = br#"{"object":"page","entry":[{"id":"page-1","messaging":[{"sender":{"id":"page-1"},"message":{"mid":"m1","text":"echo","is_echo":true}}]}]}"#;
        assert!(parse_meta_payload(echo).unwrap().is_empty());
    }

    #[tokio::test]
    async fn receive_refuses_missing_or_invalid_signatures() {
        std::env::remove_var("NAVIGATOR_META_WEBHOOK_APP_SECRET");
        let state = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let unavailable = receive(
            axum::extract::State(state.clone()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(unavailable, StatusCode::SERVICE_UNAVAILABLE);
        std::env::set_var("NAVIGATOR_META_WEBHOOK_APP_SECRET", "synthetic-meta-secret");
        let unauthorized = receive(
            axum::extract::State(state),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);
        std::env::remove_var("NAVIGATOR_META_WEBHOOK_APP_SECRET");
    }

    #[tokio::test]
    async fn challenge_echoes_a_matching_verify_token() {
        std::env::remove_var("NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN");
        let unavailable = challenge(axum::extract::Query(ChallengeQuery {
            mode: "subscribe".into(),
            verify_token: "synthetic-verify-token".into(),
            challenge: "challenge-body".into(),
        }))
        .await
        .into_response();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        std::env::set_var(
            "NAVIGATOR_META_WEBHOOK_VERIFY_TOKEN",
            "synthetic-verify-token",
        );
        let accepted = challenge(axum::extract::Query(ChallengeQuery {
            mode: "subscribe".into(),
            verify_token: "synthetic-verify-token".into(),
            challenge: "challenge-body".into(),
        }))
        .await
        .into_response();
        assert_eq!(accepted.status(), StatusCode::OK);
        let rejected = challenge(axum::extract::Query(ChallengeQuery {
            mode: "subscribe".into(),
            verify_token: "wrong".into(),
            challenge: "challenge-body".into(),
        }))
        .await
        .into_response();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn receive_ingests_a_signed_instagram_comment_for_one_connected_account() {
        use store::brands::NewBrand;
        use store::firms::NewFirm;
        use store::persons::{NewPerson, Role};
        use store::social_accounts::{ConnectSocialAccount, SocialProvider};
        use store::test_support::{self, mem_surreal};
        use uuid::Uuid;

        let db = mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &NewPerson::with_role(
                "Webhook Admin",
                format!("webhook-admin-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &NewFirm {
                name: "Webhook Firm".into(),
                status: "active".into(),
                entity_id: test_support::seed_entity(&db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        store::brands::create(
            &db,
            Role::Admin,
            Some(admin.id),
            &NewBrand {
                name: "Webhook Brand".into(),
                key: "webhook-brand".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let kms = cloud::FakeKms::new("webhook-social");
        store::social_accounts::connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin.id,
                brand_key: "webhook-brand",
                provider: SocialProvider::Instagram,
                provider_account_id: "ig-account",
                display_name: Some("Webhook IG"),
                scopes: &["instagram_business_manage_comments".into()],
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            &kms,
        )
        .await
        .unwrap();

        let body = br#"{"object":"instagram","entry":[{"id":"ig-account","changes":[{"field":"comments","value":{"id":"comment-99","text":"A question","timestamp":1791392400,"media":{"id":"post-1"},"from":{"id":"person-1","username":"reader"}}}]}]}"#;
        let secret = "synthetic-meta-secret";
        std::env::set_var("NAVIGATOR_META_WEBHOOK_APP_SECRET", secret);
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let signature = mac.finalize().into_bytes();
        #[allow(clippy::format_collect)]
        let signature = signature
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(&format!("sha256={signature}")).unwrap(),
        );
        let mut state = crate::test_support::app_state(db).await;
        state.runtime_kms = std::sync::Arc::new(kms);
        let status = receive(
            axum::extract::State(state),
            headers,
            Bytes::from_static(body),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
}
