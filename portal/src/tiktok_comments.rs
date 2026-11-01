//! TikTok Accounts API public text comments. Signature and event contracts:
//! <https://business-api.tiktok.com/portal/docs/webhook-verification/v1.3>
//! <https://business-api.tiktok.com/portal/docs/comment-update-event/v1.3>

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use store::{
    config::SocialEgressGate,
    inbox::{OutboundEnvelope, TransportOutcome},
    social_accounts::SocialProvider,
};

const BASE: &str = "https://business-api.tiktok.com/open_api/v1.3";
const MAX_BYTES: usize = 64 * 1024;

#[derive(Deserialize)]
struct WebhookEvent {
    client_key: String,
    event: String,
    user_openid: String,
    content: String,
}

#[derive(Deserialize)]
struct CommentEvent {
    comment_id: u64,
    video_id: u64,
    comment_action: String,
}

#[derive(Deserialize)]
struct AccountComment {
    comment_id: String,
    video_id: String,
    parent_comment_id: Option<String>,
    unique_identifier: String,
    display_name: Option<String>,
    #[serde(default)]
    text: String,
    create_time: String,
    owner: bool,
    status: String,
}

#[derive(Deserialize)]
struct CommentPage {
    comments: Vec<AccountComment>,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    code: i64,
    data: Option<T>,
}

fn verify(secret: &str, headers: &HeaderMap, body: &[u8], now: i64) -> bool {
    let Some(header) = headers
        .get("tiktok-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mut timestamp = None;
    let mut signature = None;
    for entry in header.split(',') {
        let Some((key, value)) = entry.trim().split_once('=') else {
            return false;
        };
        match key {
            "t" if timestamp.is_none() => timestamp = Some(value),
            "s" if signature.is_none() => signature = Some(value),
            _ => return false,
        }
    }
    let (Some(timestamp), Some(signature)) = (timestamp, signature) else {
        return false;
    };
    let Ok(seconds) = timestamp.parse::<i64>() else {
        return false;
    };
    if now.abs_diff(seconds) > 300 || signature.len() != 64 || secret.is_empty() {
        return false;
    }
    let mut decoded = [0_u8; 32];
    let (pairs, _) = signature.as_bytes().as_chunks::<2>();
    for (byte, pair) in decoded.iter_mut().zip(pairs) {
        let Ok(pair) = std::str::from_utf8(pair) else {
            return false;
        };
        let Ok(value) = u8::from_str_radix(pair, 16) else {
            return false;
        };
        *byte = value;
    }
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    mac.verify_slice(&decoded).is_ok()
}

async fn read<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, StatusCode> {
    if !response.status().is_success() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    if bytes.len() > MAX_BYTES {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let result: ApiResponse<T> =
        serde_json::from_slice(&bytes).map_err(|_| StatusCode::BAD_GATEWAY)?;
    if result.code != 0 {
        return Err(StatusCode::BAD_GATEWAY);
    }
    result.data.ok_or(StatusCode::BAD_GATEWAY)
}

async fn receive(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Ok(secret) = std::env::var("NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET") else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let Ok(app_id) = std::env::var("NAVIGATOR_TIKTOK_BUSINESS_APP_ID") else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    receive_authenticated(&state, &headers, &body, &app_id, &secret).await
}

async fn receive_authenticated(
    state: &crate::AppState,
    headers: &HeaderMap,
    body: &[u8],
    app_id: &str,
    secret: &str,
) -> StatusCode {
    if !verify(secret, headers, body, Utc::now().timestamp()) {
        return StatusCode::UNAUTHORIZED;
    }
    let event = match serde_json::from_slice::<WebhookEvent>(body) {
        Ok(event) if event.client_key == app_id => event,
        _ => return StatusCode::BAD_REQUEST,
    };
    if event.event != "comment.update" {
        return StatusCode::OK;
    }
    let Ok(comment) = serde_json::from_str::<CommentEvent>(&event.content) else {
        return StatusCode::BAD_REQUEST;
    };
    // Historical transcript entries remain records of what was received.
    // Visibility/moderation and deletion events never create public replies.
    if !matches!(comment.comment_action.as_str(), "insert" | "set_to_public") {
        return StatusCode::OK;
    }
    let result = receive_comment(
        state,
        &reqwest::Client::new(),
        BASE,
        &event.user_openid,
        &comment,
        &SocialEgressGate::from_process_env(),
    )
    .await;
    match result {
        Ok(()) => StatusCode::OK,
        Err(status) => {
            if status.is_server_error() {
                tracing::warn!(
                    status = status.as_u16(),
                    "TikTok comment delivery could not be ingested"
                );
            }
            status
        }
    }
}

async fn receive_comment(
    state: &crate::AppState,
    http: &reqwest::Client,
    base: &str,
    account_id: &str,
    event: &CommentEvent,
    egress: &SocialEgressGate,
) -> Result<(), StatusCode> {
    egress
        .check()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let accounts = store::social_accounts::for_provider_identity(
        &state.surreal,
        SocialProvider::Tiktok,
        account_id,
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let [account] = accounts.as_slice() else {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    };
    if !account.scopes.iter().any(|scope| scope == "comment.list") {
        return Err(StatusCode::PRECONDITION_FAILED);
    }
    let credential = workflows::social_publish::send_credential(
        &state.surreal,
        state.runtime_kms.as_ref(),
        http,
        &workflows::social_publish::TokenRefreshConfig::from_env(),
        &account.brand_key,
        SocialProvider::Tiktok,
        account_id,
    )
    .await
    .map_err(|_| StatusCode::PRECONDITION_FAILED)?;
    // Fetch the comment's current public status and owner flag. The webhook alone
    // does not distinguish an audience comment from the Brand's own reply echo.
    let page: CommentPage = read(
        http.get(format!("{base}/business/comment/list/"))
            .timeout(std::time::Duration::from_secs(15))
            .header("Access-Token", credential.access_token())
            .query(&[
                ("business_id", account_id.to_string()),
                ("video_id", event.video_id.to_string()),
                ("comment_ids", format!("[\"{}\"]", event.comment_id)),
                ("status", "PUBLIC".into()),
            ])
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?,
    )
    .await?;
    for comment in page.comments {
        if comment.comment_id != event.comment_id.to_string()
            || comment.video_id != event.video_id.to_string()
        {
            return Err(StatusCode::BAD_GATEWAY);
        }
        ingest_comment(state, account, account_id, &comment).await?;
    }
    Ok(())
}

async fn ingest_comment(
    state: &crate::AppState,
    account: &store::social_accounts::SocialAccount,
    account_id: &str,
    comment: &AccountComment,
) -> Result<(), StatusCode> {
    if comment.unique_identifier.trim().is_empty() {
        return Err(StatusCode::BAD_GATEWAY);
    }
    if comment.owner || comment.status != "PUBLIC" || comment.text.trim().is_empty() {
        return Ok(());
    }
    let Some(occurred_at) = comment
        .create_time
        .parse::<i64>()
        .ok()
        .and_then(|time| DateTime::from_timestamp(time, 0))
    else {
        return Err(StatusCode::BAD_GATEWAY);
    };
    let received = store::inbox::ingest_inbound(
        &state.surreal,
        &store::inbox::IngestInbound {
            project_id: None,
            firm_id: account.firm_id,
            brand_key: Some(&account.brand_key),
            interaction_type: Some("comment"),
            provider_post_id: Some(&comment.video_id),
            parent_provider_message_id: comment.parent_comment_id.as_deref(),
            visibility: Some("public"),
            channel: "tiktok",
            provider_account: account_id,
            provider_message_id: &comment.comment_id,
            provider_thread_ref: Some(&comment.video_id),
            external_address: &comment.unique_identifier,
            external_name: comment.display_name.as_deref(),
            subject: None,
            body: &comment.text,
            source_page: Some(account_id),
            source_channel: Some("tiktok"),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: &occurred_at.to_rfc3339(),
        },
    )
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // The alert workflow is keyed by Message ID. A repeated delivery must
    // retry durable submission if the first submission failed after ingest.
    crate::inbox::start_inbound_message_alert(received.message_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(())
}

// Transcript visibility is historical; check current visibility before writing.
async fn parent_is_public(
    http: &reqwest::Client,
    base: &str,
    account: &str,
    video_id: &str,
    comment_id: &str,
    token: &str,
) -> Result<bool, StatusCode> {
    let page: CommentPage = read(
        http.get(format!("{base}/business/comment/list/"))
            .timeout(std::time::Duration::from_secs(15))
            .header("Access-Token", token)
            .query(&[
                ("business_id", account),
                ("video_id", video_id),
                ("comment_ids", &format!("[\"{comment_id}\"]")),
                ("status", "PUBLIC"),
            ])
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?,
    )
    .await?;
    Ok(page.comments.len() == 1
        && page.comments.iter().any(|parent| {
            parent.comment_id == comment_id
                && parent.video_id == video_id
                && parent.status == "PUBLIC"
        }))
}

pub(crate) async fn send_reply(
    http: &reqwest::Client,
    base: &str,
    envelope: &OutboundEnvelope,
    token: &str,
) -> Result<TransportOutcome, String> {
    let reject = |detail: &str| TransportOutcome::Rejected {
        detail: detail.into(),
    };
    if envelope.visibility.as_deref() != Some("public") || envelope.interaction_type != "comment" {
        return Ok(reject("TikTok supports public comment replies only"));
    }
    let (Some(video_id), Some(comment_id)) = (
        &envelope.provider_post_id,
        &envelope.reply_to_provider_message_id,
    ) else {
        return Ok(reject(
            "TikTok reply requires the video and parent comment references",
        ));
    };
    if video_id.is_empty() || comment_id.is_empty() {
        return Ok(reject(
            "TikTok reply requires nonempty video and comment references",
        ));
    }
    if envelope.body.trim().is_empty() || envelope.body.chars().count() > 1200 {
        return Ok(reject(
            "TikTok comment replies must contain 1 to 1,200 characters",
        ));
    }
    match parent_is_public(http, base, &envelope.account, video_id, comment_id, token).await {
        Ok(true) => {}
        Ok(false) => {
            return Ok(reject(
                "TikTok parent comment is no longer public; the reply was not sent",
            ))
        }
        Err(_) => {
            return Ok(reject(
                "TikTok parent comment could not be verified; the reply was not sent",
            ))
        }
    }
    let response = http.post(format!("{base}/business/comment/reply/create/"))
        .timeout(std::time::Duration::from_secs(15)).header("Access-Token", token)
        .json(&serde_json::json!({"business_id":envelope.account,"video_id":video_id,"comment_id":comment_id,"text":envelope.body}))
        .send().await;
    let Ok(response) = response else {
        return Ok(TransportOutcome::Unknown);
    };
    let status = response.status();
    let Ok(body) = response.bytes().await else {
        return Ok(TransportOutcome::Unknown);
    };
    if body.len() > MAX_BYTES {
        return Ok(TransportOutcome::Unknown);
    }
    if status.is_server_error() || status.as_u16() == 429 {
        return Ok(TransportOutcome::Unknown);
    }
    if !status.is_success() {
        return Ok(reject("TikTok rejected the comment reply"));
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Ok(TransportOutcome::Unknown);
    };
    match value.get("code").and_then(serde_json::Value::as_i64) {
        Some(0) => match value
            .pointer("/data/comment_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        {
            Some(id) => Ok(TransportOutcome::Submitted {
                provider_ref: id.into(),
            }),
            None => Ok(TransportOutcome::Unknown),
        },
        Some(_) => Ok(reject(
            "TikTok refused the reply; verify the account comment permissions",
        )),
        None => Ok(TransportOutcome::Unknown),
    }
}

pub(crate) fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/webhook/tiktok", post(receive))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BYTES))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{body_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn signed(secret: &str, body: &[u8], timestamp: i64) -> HeaderMap {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        let signature =
            mac.finalize()
                .into_bytes()
                .iter()
                .fold(String::new(), |mut output, byte| {
                    use std::fmt::Write;
                    write!(output, "{byte:02x}").unwrap();
                    output
                });
        let mut headers = HeaderMap::new();
        headers.insert(
            "tiktok-signature",
            format!("t={timestamp},s={signature}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn signature_authenticates_body_timestamp_and_rejects_stale_signatures() {
        let body = br#"{"event":"comment.update"}"#;
        let secret = uuid::Uuid::new_v4().to_string();
        let wrong_secret = uuid::Uuid::new_v4().to_string();
        let headers = signed(&secret, body, 1_800_000_000);
        assert!(verify(&secret, &headers, body, 1_800_000_001));
        assert!(!verify(&wrong_secret, &headers, body, 1_800_000_001));
        assert!(!verify(&secret, &headers, b"{}", 1_800_000_001));
        assert!(!verify(&secret, &headers, body, 1_800_000_301));
        assert!(!verify(&secret, &headers, body, 1_799_999_699));
        let mut duplicate = headers.clone();
        duplicate.insert(
            "tiktok-signature",
            "t=1800000000,t=1800000000,s=00".parse().unwrap(),
        );
        assert!(!verify(&secret, &duplicate, body, 1_800_000_000));
    }

    #[test]
    fn webhook_ids_above_javascript_integer_precision_remain_exact() {
        let event: CommentEvent = serde_json::from_str(r#"{"comment_id":7247303576418566913,"video_id":7203946942097902849,"comment_action":"insert"}"#).unwrap();
        assert_eq!(event.comment_id.to_string(), "7247303576418566913");
        assert_eq!(event.video_id.to_string(), "7203946942097902849");
    }

    fn envelope() -> OutboundEnvelope {
        OutboundEnvelope {
            channel: "tiktok".into(),
            account: "synthetic-account".into(),
            interaction_type: "comment".into(),
            provider_post_id: Some("7203946942097902849".into()),
            reply_to_provider_message_id: Some("7247303576418566913".into()),
            provider_thread_ref: None,
            to: "synthetic-reader".into(),
            subject: None,
            body: "Thanks for your question.".into(),
            purpose: "matter".into(),
            visibility: Some("public".into()),
        }
    }

    async fn public_parent(server: &MockServer) {
        Mock::given(method("GET")).and(path("/business/comment/list/"))
            .and(wiremock::matchers::query_param("business_id", "synthetic-account"))
            .and(wiremock::matchers::query_param("video_id", "7203946942097902849"))
            .and(wiremock::matchers::query_param("comment_ids", "[\"7247303576418566913\"]"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[{
                "comment_id":"7247303576418566913","video_id":"7203946942097902849","unique_identifier":"synthetic-reader",
                "text":"A public question","create_time":"1791392400","owner":false,"status":"PUBLIC"
            }]}}))).expect(1).mount(server).await;
    }

    #[tokio::test]
    async fn reply_sends_owned_video_context_and_requires_a_confirmed_reply_id() {
        let server = MockServer::start().await;
        public_parent(&server).await;
        Mock::given(method("POST")).and(path("/business/comment/reply/create/"))
            .and(header("access-token", "synthetic-token"))
            .and(body_json(serde_json::json!({"business_id":"synthetic-account","video_id":"7203946942097902849",
                "comment_id":"7247303576418566913","text":"Thanks for your question."})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comment_id":"7247303576418566999"}})))
            .expect(1).mount(&server).await;
        assert_eq!(
            send_reply(
                &reqwest::Client::new(),
                &server.uri(),
                &envelope(),
                "synthetic-token"
            )
            .await
            .unwrap(),
            TransportOutcome::Submitted {
                provider_ref: "7247303576418566999".into()
            }
        );
    }

    #[tokio::test]
    async fn reply_refuses_private_missing_context_and_oversized_text_before_network() {
        let server = MockServer::start().await;
        let mut private = envelope();
        private.visibility = Some("private".into());
        let mut missing = envelope();
        missing.provider_post_id = None;
        let mut oversized = envelope();
        oversized.body = "a".repeat(1201);
        for envelope in [private, missing, oversized] {
            assert!(matches!(
                send_reply(
                    &reqwest::Client::new(),
                    &server.uri(),
                    &envelope,
                    "synthetic-token"
                )
                .await
                .unwrap(),
                TransportOutcome::Rejected { .. }
            ));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn successful_http_without_a_provider_receipt_is_unknown() {
        for body in [
            serde_json::json!({"code":0,"data":{}}),
            serde_json::json!({"message":"OK"}),
        ] {
            let server = MockServer::start().await;
            public_parent(&server).await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                send_reply(
                    &reqwest::Client::new(),
                    &server.uri(),
                    &envelope(),
                    "synthetic-token"
                )
                .await
                .unwrap(),
                TransportOutcome::Unknown
            );
        }
    }
    async fn state_with_account(scopes: Vec<String>) -> crate::AppState {
        use store::{
            brands::NewBrand,
            firms::NewFirm,
            persons::{NewPerson, Role},
            social_accounts::ConnectSocialAccount,
        };
        let db = store::test_support::mem_surreal().await;
        let admin = store::persons::create(
            &db,
            &NewPerson::with_role("Synthetic Admin", "tiktok-test@example.com", Role::Admin),
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
                key: "synthetic-tiktok".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        let kms = std::sync::Arc::new(cloud::FakeKms::new("tiktok-test"));
        store::social_accounts::connect(
            &db,
            ConnectSocialAccount {
                actor_role: Role::Admin,
                actor_person_id: admin.id,
                brand_key: "synthetic-tiktok",
                provider: SocialProvider::Tiktok,
                provider_account_id: "synthetic-account",
                display_name: None,
                scopes: &scopes,
                expires_at: None,
                access_token: "synthetic-token",
                refresh_token: None,
            },
            kms.as_ref(),
        )
        .await
        .unwrap();
        let mut state = crate::test_support::app_state(db).await;
        state.runtime_kms = kms;
        state
    }

    #[tokio::test]
    async fn inbound_public_comment_is_idempotent_and_owner_echo_is_ignored() {
        let mut scopes = SocialProvider::Tiktok
            .required_scopes()
            .iter()
            .map(|scope| (*scope).into())
            .collect::<Vec<String>>();
        scopes.push("comment.list".into());
        let state = state_with_account(scopes).await;
        let server = MockServer::start().await;
        let event = CommentEvent {
            comment_id: 7_247_303_576_418_566_913,
            video_id: 7_203_946_942_097_902_849,
            comment_action: "insert".into(),
        };
        Mock::given(method("GET")).and(path("/business/comment/list/"))
            .and(header("Access-Token", "synthetic-token"))
            .and(wiremock::matchers::query_param("comment_ids", "[\"7247303576418566913\"]"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[{
                "comment_id":"7247303576418566913","video_id":"7203946942097902849","unique_identifier":"synthetic-reader",
                "display_name":"Synthetic Reader","text":"A public question","create_time":"1791392400","owner":false,"status":"PUBLIC"
            }]}}))).expect(2).mount(&server).await;
        let gate = SocialEgressGate::new(|| Ok(()));
        for _ in 0..2 {
            receive_comment(
                &state,
                &reqwest::Client::new(),
                &server.uri(),
                "synthetic-account",
                &event,
                &gate,
            )
            .await
            .unwrap();
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
        assert_eq!(messages[0]["body"], "A public question");
        server.reset().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[{
            "comment_id":"7247303576418566913","video_id":"7203946942097902849","unique_identifier":"synthetic-brand-owner",
            "text":"Our own reply","create_time":"1791392400","owner":true,"status":"PUBLIC"
        }]}}))).mount(&server).await;
        receive_comment(
            &state,
            &reqwest::Client::new(),
            &server.uri(),
            "synthetic-account",
            &event,
            &gate,
        )
        .await
        .unwrap();
        let mut response = state
            .surreal
            .query("SELECT * FROM message")
            .await
            .unwrap()
            .check()
            .unwrap();
        let messages: Vec<serde_json::Value> = response.take(0).unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[tokio::test]
    async fn inbound_requires_comment_read_grant_before_fetching() {
        let scopes = SocialProvider::Tiktok
            .required_scopes()
            .iter()
            .map(|scope| (*scope).into())
            .collect();
        let state = state_with_account(scopes).await;
        let server = MockServer::start().await;
        let event = CommentEvent {
            comment_id: 1,
            video_id: 2,
            comment_action: "insert".into(),
        };
        assert_eq!(
            receive_comment(
                &state,
                &reqwest::Client::new(),
                &server.uri(),
                "synthetic-account",
                &event,
                &SocialEgressGate::new(|| Ok(()))
            )
            .await,
            Err(StatusCode::PRECONDITION_FAILED)
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn signed_nonpublic_events_acknowledge_without_new_messages_and_wrong_app_is_refused() {
        let state = crate::test_support::app_state(store::test_support::mem_surreal().await).await;
        let secret = uuid::Uuid::new_v4().to_string();
        for action in ["delete", "set_to_hidden", "set_to_friends_only"] {
            let body = serde_json::to_vec(&serde_json::json!({"client_key":"synthetic-app","event":"comment.update","user_openid":"synthetic-account",
                "content":serde_json::json!({"comment_id":1,"video_id":2,"comment_action":action}).to_string()})).unwrap();
            let headers = signed(&secret, &body, Utc::now().timestamp());
            assert_eq!(
                receive_authenticated(&state, &headers, &body, "synthetic-app", &secret).await,
                StatusCode::OK
            );
            assert_eq!(
                receive_authenticated(&state, &headers, &body, "another-app", &secret).await,
                StatusCode::BAD_REQUEST
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
        assert!(messages.is_empty());
    }
    #[tokio::test]
    async fn reply_never_posts_to_a_hidden_deleted_or_unverifiable_parent() {
        for response in [
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[]}})),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[{
                "comment_id":"7247303576418566913","video_id":"7203946942097902849","unique_identifier":"synthetic-reader",
                "text":"A hidden question","create_time":"1791392400","owner":false,"status":"HIDDEN"
            }]}})),
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"comments":[{
                "comment_id":"7247303576418566913","video_id":"another-video","unique_identifier":"synthetic-reader",
                "text":"Wrong video","create_time":"1791392400","owner":false,"status":"PUBLIC"
            }]}})),
            ResponseTemplate::new(503),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).respond_with(response).expect(1).mount(&server).await;
            assert!(matches!(send_reply(&reqwest::Client::new(), &server.uri(), &envelope(), "synthetic-token").await.unwrap(), TransportOutcome::Rejected { .. }));
            assert!(server.received_requests().await.unwrap().iter().all(|request| request.method.as_str() == "GET"));
        }
    }
}
