//! X Account Activity v2 inbound webhooks. Signature and CRC contracts:
//! <https://docs.x.com/x-api/account-activity/introduction>
//!
//! Registration exchanges a Challenge-Response Check (`GET` echoing an HMAC of
//! `crc_token`); every delivery carries `X-Twitter-Webhooks-Signature-OAuth2`,
//! an HMAC-SHA256 over the exact raw body, verified in constant time. Mentions
//! and direct messages file into the firm inbox on channel `x` through the
//! same ingest seam every other social channel uses; the webhook payload is
//! self-contained, so no provider read-back is needed.

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Router,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use store::social_accounts::SocialProvider;

const MAX_BYTES: usize = 64 * 1024;
/// X renders `created_at` as `Wed Oct 10 20:19:24 +0000 2018`.
const TWEET_TIME: &str = "%a %b %d %H:%M:%S %z %Y";

const SIGNATURE_HEADER: &str = "x-twitter-webhooks-signature-oauth2";

/// The CRC response X expects at registration: `sha256=` plus the base64
/// HMAC-SHA256 of the challenge token under the app's client secret.
fn response_token(secret: &str, crc_token: &str) -> Option<String> {
    if secret.is_empty() || crc_token.is_empty() {
        return None;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(crc_token.as_bytes());
    Some(format!(
        "sha256={}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    ))
}

/// `X-Twitter-Webhooks-Signature-OAuth2` over the exact raw body, compared
/// in constant time by `verify_slice`.
fn verify_signature(secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
    use base64::Engine as _;
    let Some(value) = headers
        .get(SIGNATURE_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("sha256=") else {
        return false;
    };
    let Ok(signature) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

#[derive(Deserialize)]
struct ChallengeQuery {
    crc_token: Option<String>,
}

/// One Account Activity delivery. Unknown event classes are absent keys here
/// and are acknowledged without ingesting anything.
#[derive(Deserialize)]
struct Activity {
    for_user_id: String,
    #[serde(default)]
    tweet_create_events: Vec<Tweet>,
    #[serde(default)]
    direct_message_events: Vec<DmEvent>,
}

#[derive(Deserialize)]
struct Tweet {
    id_str: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    full_text: Option<String>,
    #[serde(default)]
    user: Option<TwitterUser>,
    #[serde(default)]
    in_reply_to_status_id_str: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct TwitterUser {
    id_str: String,
    #[serde(default)]
    screen_name: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct DmEvent {
    id: String,
    created_timestamp: i64,
    #[serde(default)]
    message_create: Option<DmCreate>,
}

#[derive(Deserialize)]
struct DmCreate {
    sender_id: String,
    #[serde(default)]
    message_data: Option<DmData>,
}

#[derive(Deserialize)]
struct DmData {
    id: String,
    #[serde(default)]
    text: String,
}

/// `GET /webhook/x` subscription challenge.
async fn challenge(Query(query): Query<ChallengeQuery>) -> (StatusCode, String) {
    let Ok(secret) = std::env::var("NAVIGATOR_X_CLIENT_SECRET") else {
        return (StatusCode::SERVICE_UNAVAILABLE, String::new());
    };
    let Some(token) = query.crc_token.filter(|token| !token.is_empty()) else {
        return (StatusCode::BAD_REQUEST, String::new());
    };
    match response_token(&secret, &token) {
        Some(token) => (
            StatusCode::OK,
            format!(r#"{{"response_token":"{token}"}}"#),
        ),
        None => (StatusCode::SERVICE_UNAVAILABLE, String::new()),
    }
}

/// `POST /webhook/x` verified Account Activity deliveries.
async fn receive(State(state): State<crate::AppState>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Ok(secret) = std::env::var("NAVIGATOR_X_CLIENT_SECRET") else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    receive_authenticated(&state, &headers, &body, &secret)
        .await
        .unwrap_or_else(|status| {
            if status.is_server_error() {
                tracing::warn!(
                    status = status.as_u16(),
                    "X activity delivery could not be ingested"
                );
            }
            status
        })
}

async fn receive_authenticated(
    state: &crate::AppState,
    headers: &HeaderMap,
    body: &[u8],
    secret: &str,
) -> Result<StatusCode, StatusCode> {
    if !verify_signature(secret, headers, body) {
        return Ok(StatusCode::UNAUTHORIZED);
    }
    let activity = serde_json::from_slice::<Activity>(body)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    ingest_activity(state, &activity).await
}

async fn ingest_activity(
    state: &crate::AppState,
    activity: &Activity,
) -> Result<StatusCode, StatusCode> {
    // Webhook registration is per X account: `for_user_id` is the receiving
    // account the connected Social Account was authorized against.
    let accounts = store::social_accounts::for_provider_identity(
        &state.surreal,
        SocialProvider::X,
        &activity.for_user_id,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let [account] = accounts.as_slice() else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };
    for tweet in &activity.tweet_create_events {
        ingest_tweet(state, account, &activity.for_user_id, tweet).await?;
    }
    for event in &activity.direct_message_events {
        ingest_dm(state, account, &activity.for_user_id, event).await?;
    }
    // A delivery carrying only unsupported event classes (likes, follows,
    // blocks) is acknowledged: nothing to file, nothing to retry.
    Ok(StatusCode::OK)
}

async fn ingest_tweet(
    state: &crate::AppState,
    account: &store::social_accounts::SocialAccount,
    for_user_id: &str,
    tweet: &Tweet,
) -> Result<(), StatusCode> {
    let Some(user) = tweet.user.as_ref() else {
        return Ok(());
    };
    // The Brand's own tweets echo through the activity stream; they are
    // outbound publishing, never inbound messages.
    if user.id_str == for_user_id {
        return Ok(());
    }
    let body = tweet
        .full_text
        .as_deref()
        .unwrap_or(&tweet.text)
        .trim()
        .to_string();
    if body.is_empty() {
        return Ok(());
    }
    let occurred = DateTime::parse_from_str(&tweet.created_at, TWEET_TIME)
        .map_err(|_| StatusCode::BAD_GATEWAY)?
        .with_timezone(&Utc);
    received(
        state,
        for_user_id,
        &store::inbox::IngestInbound {
            firm_id: account.firm_id,
            brand_key: Some(&account.brand_key),
            interaction_type: Some("comment"),
            provider_post_id: tweet.in_reply_to_status_id_str.as_deref().or(Some(&tweet.id_str)),
            parent_provider_message_id: tweet.in_reply_to_status_id_str.as_deref(),
            visibility: Some("public"),
            channel: "x",
            provider_account: for_user_id,
            provider_message_id: &tweet.id_str,
            provider_thread_ref: Some(&tweet.id_str),
            external_address: &user.id_str,
            external_name: Some(if user.screen_name.is_empty() {
                user.name.as_deref().unwrap_or(&user.id_str)
            } else {
                &user.screen_name
            }),
            subject: None,
            body: &body,
            source_page: Some(for_user_id),
            source_channel: Some("x"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &occurred.to_rfc3339(),
        },
    )
    .await
}

async fn ingest_dm(
    state: &crate::AppState,
    account: &store::social_accounts::SocialAccount,
    for_user_id: &str,
    event: &DmEvent,
) -> Result<(), StatusCode> {
    let Some(create) = event.message_create.as_ref() else {
        return Ok(());
    };
    // The Brand's own DM sends echo through the activity stream too.
    if create.sender_id == for_user_id {
        return Ok(());
    }
    let Some(data) = create.message_data.as_ref() else {
        return Ok(());
    };
    let body = data.text.trim().to_string();
    if body.is_empty() {
        return Ok(());
    }
    let Some(occurred) = DateTime::from_timestamp_millis(event.created_timestamp) else {
        return Err(StatusCode::BAD_GATEWAY);
    };
    received(
        state,
        for_user_id,
        &store::inbox::IngestInbound {
            firm_id: account.firm_id,
            brand_key: Some(&account.brand_key),
            interaction_type: Some("direct_message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: Some("private"),
            channel: "x",
            provider_account: for_user_id,
            provider_message_id: &data.id,
            provider_thread_ref: Some(&event.id),
            external_address: &create.sender_id,
            external_name: None,
            subject: None,
            body: &body,
            source_page: Some(for_user_id),
            source_channel: Some("x"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachment_asset_id: None,
            occurred_at: &occurred.to_rfc3339(),
        },
    )
    .await
}

/// File the message, then start the same alert and triage workflows every
/// other social channel runs. A repeated provider delivery dedups on the
/// provider message id and must not re-file, but the durable workflow
/// submissions are keyed by Message ID and retry safely on replay.
async fn received(
    state: &crate::AppState,
    for_user_id: &str,
    inbound: &store::inbox::IngestInbound<'_>,
) -> Result<(), StatusCode> {
    let received = store::inbox::ingest_inbound(&state.surreal, inbound)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::inbox::start_inbound_message_alert(received.message_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    crate::inbox::start_inbound_message_triage(received.message_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tracing::info!(
        channel = "x",
        message_id = %received.message_id,
        deduped = received.deduped,
        account = for_user_id,
        "X activity ingested"
    );
    Ok(())
}

pub(crate) fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/webhook/x", get(challenge).post(receive))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use base64::engine::general_purpose::STANDARD;
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use store::{brands::NewBrand, firms::NewFirm, persons::NewPerson, persons::Role};

    fn signed(secret: &str, body: &[u8]) -> HeaderMap {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let mut headers = HeaderMap::new();
        headers.insert(
            SIGNATURE_HEADER,
            format!(
                "sha256={}",
                STANDARD.encode(mac.finalize().into_bytes())
            )
            .parse()
            .unwrap(),
        );
        headers
    }

    fn crc(secret: &str, token: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(token.as_bytes());
        format!("sha256={}", STANDARD.encode(mac.finalize().into_bytes()))
    }

    async fn state_with_account() -> crate::AppState {
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &NewPerson::with_role("Synthetic Admin", "x-test@example.com", Role::Admin),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            &db,
            &NewFirm {
                name: "Synthetic Firm".into(),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(&db).await,
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
                name: "Synthetic Brand".into(),
                key: "synthetic-x".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let kms = std::sync::Arc::new(cloud::FakeKms::new("x-test"));
        store::social_accounts::connect(
            &db,
            store::social_accounts::ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin.id,
                brand_key: "synthetic-x",
                provider: SocialProvider::X,
                provider_account_id: "1200000000000000001",
                display_name: None,
                scopes: &SocialProvider::X.required_scopes()
                    .iter()
                    .map(|scope| (*scope).to_string())
                    .collect::<Vec<String>>(),
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            kms.as_ref(),
        )
        .await
        .unwrap();
        crate::test_support::app_state(db).await
    }

    fn mention_body(for_user: &str, id: &str, sender: &str, text: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "for_user_id": for_user,
            "tweet_create_events": [{
                "id_str": id,
                "text": text,
                "user": {"id_str": sender, "screen_name": "synthetic-reader", "name": "Synthetic Reader"},
                "created_at": "Wed Oct 29 20:19:24 +0000 2025"
            }]
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn mention_ingests_with_full_attribution_and_replays_dedup() {
        let state = state_with_account().await;
        let body = mention_body("1200000000000000001", "1800000000000000001", "5500000000000000001", "A public mention");
        let headers = signed("synthetic-secret", &body);
        for _ in 0..2 {
            assert_eq!(
                receive_authenticated(&state, &headers, &body, "synthetic-secret")
                    .await
                    .unwrap(),
                StatusCode::OK
            );
        }
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["body"], "A public mention");
        let mut response = state
            .surreal
            .query("SELECT * FROM inbox_conversation")
            .await
            .unwrap()
            .check()
            .unwrap();
        let conversations: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0]["channel"], "x");
        assert_eq!(conversations[0]["provider_account"], "1200000000000000001");
        assert_eq!(conversations[0]["external_address"], "5500000000000000001");
        assert_eq!(conversations[0]["external_name"], "synthetic-reader");
        assert_eq!(conversations[0]["brand_key"], "synthetic-x");
    }

    #[tokio::test]
    async fn unsigned_tampered_and_wrong_secret_deliveries_are_refused() {
        let state = state_with_account().await;
        let body = mention_body("1200000000000000001", "1800000000000000002", "5500000000000000001", "Unsigned");
        assert_eq!(
            receive_authenticated(&state, &HeaderMap::new(), &body, "synthetic-secret")
                .await
                .unwrap(),
            StatusCode::UNAUTHORIZED
        );
        let headers = signed("synthetic-secret", &body);
        assert_eq!(
            receive_authenticated(&state, &headers, b"tampered", "synthetic-secret")
                .await
                .unwrap(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            receive_authenticated(&state, &headers, &body, "another-secret")
                .await
                .unwrap(),
            StatusCode::UNAUTHORIZED
        );
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn brand_echoes_and_unknown_events_acknowledge_without_filing() {
        let state = state_with_account().await;
        let echo = serde_json::to_vec(&serde_json::json!({
            "for_user_id": "1200000000000000001",
            "tweet_create_events": [{
                "id_str": "1800000000000000003",
                "text": "Our own tweet",
                "user": {"id_str": "1200000000000000001", "screen_name": "syntheticbrand"},
                "created_at": "Wed Oct 29 20:19:24 +0000 2025"
            }]
        }))
        .unwrap();
        let headers = signed("synthetic-secret", &echo);
        assert_eq!(
            receive_authenticated(&state, &headers, &echo, "synthetic-secret")
                .await
                .unwrap(),
            StatusCode::OK
        );
        let likes = serde_json::to_vec(&serde_json::json!({
            "for_user_id": "1200000000000000001",
            "favorite_events": [{"favorited_status": {"id_str": "1"}}]
        }))
        .unwrap();
        let headers = signed("synthetic-secret", &likes);
        assert_eq!(
            receive_authenticated(&state, &headers, &likes, "synthetic-secret")
                .await
                .unwrap(),
            StatusCode::OK
        );
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn direct_messages_file_as_private_direct_message_threads() {
        let state = state_with_account().await;
        let body = serde_json::to_vec(&serde_json::json!({
            "for_user_id": "1200000000000000001",
            "direct_message_events": [{
                "id": "1900000000000000001",
                "created_timestamp": 1_791_392_400_000_i64,
                "message_create": {
                    "sender_id": "5500000000000000002",
                    "message_data": {"id": "1900000000000000002", "text": "A direct message"}
                }
            }]
        }))
        .unwrap();
        let headers = signed("synthetic-secret", &body);
        assert_eq!(
            receive_authenticated(&state, &headers, &body, "synthetic-secret")
                .await
                .unwrap(),
            StatusCode::OK
        );
        let mut response = state
            .surreal
            .query("SELECT * FROM inbox_conversation")
            .await
            .unwrap()
            .check()
            .unwrap();
        let conversations: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(conversations.len(), 1);
        assert_eq!(conversations[0]["interaction_type"], "direct_message");
        assert_eq!(conversations[0]["external_address"], "5500000000000000002");
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["provider_message_id"], "1900000000000000002");
    }

    #[tokio::test]
    async fn an_unknown_receiving_account_is_unprocessable() {
        let state = state_with_account().await;
        let body = mention_body("9999999999999999999", "1800000000000000004", "5500000000000000001", "For someone else");
        let headers = signed("synthetic-secret", &body);
        assert_eq!(
            receive_authenticated(&state, &headers, &body, "synthetic-secret")
                .await
                .unwrap_err(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn crc_tokens_are_hmacs_of_the_challenge() {
        assert_eq!(
            crc("synthetic-secret", "challenge-token"),
            response_token("synthetic-secret", "challenge-token").unwrap()
        );
        assert_ne!(
            crc("synthetic-secret", "challenge-token"),
            response_token("another-secret", "challenge-token").unwrap()
        );
        assert!(response_token("", "challenge-token").is_none());
    }

    #[test]
    fn signature_header_shapes_are_exact() {
        let body = b"payload";
        let headers = signed("synthetic-secret", body);
        assert!(verify_signature("synthetic-secret", &headers, body));
        let mut missing_prefix = headers.clone();
        missing_prefix.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(&STANDARD.encode([0_u8; 32])).unwrap(),
        );
        assert!(!verify_signature("synthetic-secret", &missing_prefix, body));
        let mut short = headers.clone();
        short.insert(SIGNATURE_HEADER, "sha256=YQ==".parse().unwrap());
        assert!(!verify_signature("synthetic-secret", &short, body));
    }
}
