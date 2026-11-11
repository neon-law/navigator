//! The push pipeline end to end: a verified push through the receiver's
//! mounted route reaches the Firm's Forgejo and Slack over wiremock —
//! one verified push produces one posted Block Kit message; an
//! unconfigured Slack credential records a failed receipt with the
//! error named and loses nothing; a redelivery after a delivery failure
//! re-runs only the unfinished tail (the persisted analysis, not the
//! Forgejo read).
//!
//! The journal is PostgreSQL-backed, so each test owns an isolated
//! database off the CI test container, absent locally — the caller skips
//! the assertions it cannot serve (the house fixture, as in
//! `forgejo_webhook.rs`).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use portal::forgejo_push_pipeline::{FixedPushCredentials, JjPushPipeline};
use portal::forgejo_webhook::PushConsumer;
use store::postgres::forgejo_push_webhook_journal;
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SHA: &str = "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1";
const REPO: &str = "firm-slug/project-code";
const PATH_SECRET: &str = "forgejo-test-secret";

/// The payload Forgejo sends for a jj push of one `push/<change-id>`
/// bookmark, as raw bytes — the exact bytes the HMAC must cover.
fn push_body() -> Vec<u8> {
    serde_json::json!({
        "ref": "refs/heads/push/qvrpxyq",
        "before": "0000000000000000000000000000000000000000",
        "after": SHA,
        "total_commits": 1,
        "repository": { "id": 7, "full_name": REPO },
        "pusher": { "login": "tbot" }
    })
    .to_string()
    .into_bytes()
}

/// A fresh HMAC key for one fixture, generated at runtime — no reusable
/// webhook credential ships in fixtures (the receiver tests' discipline).
fn hmac_key() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// The isolated Postgres database the pipeline's journal reads, mirroring
/// the house fixture: absent locally, so the caller skips the assertions
/// it cannot serve.
async fn database() -> Option<store::postgres::PostgresDb> {
    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").ok()?;
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("forgejo_pipeline_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = store::postgres::PostgresDb::connect(options.database(&name))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

/// The Firm's Forgejo API, mocked: one commit's detail (author, message,
/// stats) and its unified diff — the two reads `analyze_push` makes.
async fn forgejo_mock() -> MockServer {
    let server = MockServer::start().await;
    let api = format!("/api/v1/repos/{REPO}/git/commits");
    // Distinct exact paths: the diff read and the detail read, each
    // mounted with `.expect(1)` — analysis runs exactly once, and a
    // redelivery that re-read the API would violate the expectation.
    Mock::given(method("GET"))
        .and(path(format!("{api}/{SHA}.diff")))
        .respond_with(ResponseTemplate::new(200).set_body_string(concat!(
            "diff --git a/letter.md b/letter.md\n",
            "--- a/letter.md\n",
            "+++ b/letter.md\n",
            "@@ -1,2 +1,4 @@\n",
            " context\n",
            "+an added line\n",
        )))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{api}/{SHA}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "commit": {
                "author": { "name": "T-bot", "email": "tbot@neonlaw.org" },
                "message": "Amend the filing\n",
            },
            "stats": { "additions": 2, "deletions": 1 }
        })))
        .expect(1)
        .mount(&server)
        .await;
    server
}

/// The Firm's Slack workspace, mocked: no channel yet, creating one
/// succeeds, and a Block Kit post succeeds. Returns the server and the
/// mount for the chat.postMessage read — tests that script delivery
/// failures mount their own and leave this one unmounted.
async fn slack_mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/conversations.list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "channels": [],
            "response_metadata": { "next_cursor": "" }
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/conversations.create"))
        .respond_with(|request: &wiremock::Request| {
            // Slack answers a create with the channel carrying the name
            // the caller asked for, and `ensure_private_channel` holds
            // that invariant strictly. Echoing the request's name keeps
            // the fixture independent of the channel-naming prefix.
            let name = serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body["name"].as_str().map(str::to_owned))
                .unwrap_or_default();
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "channel": { "id": "C04PUSH", "name": name, "is_private": true }
            }))
        })
        .mount(&server)
        .await;
    server
}

struct Fixture {
    app: axum::Router,
    pg: store::postgres::PostgresDb,
    /// Held (never read) so the mocked Forgejo server — and its
    /// `.expect(1)` reads — live as long as the fixture.
    #[allow(dead_code)]
    forgejo: MockServer,
    slack: MockServer,
    hmac_key: String,
}

/// A router whose receiver is fully armed with the REAL pipeline tail:
/// a journaled store, a shared Surreal lane carrying the Project, and
/// both forges behind wiremock.
async fn fixture(slack_bot_token: Option<&str>) -> Option<Fixture> {
    let pg = database().await?;
    let hmac_key = hmac_key();
    let surreal = store::test_support::mem_surreal().await;
    store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "project-code".into(),
            name: "Sample Estate".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let forgejo = forgejo_mock().await;
    let slack = slack_mock().await;

    let consumer: std::sync::Arc<dyn PushConsumer> =
        std::sync::Arc::new(JjPushPipeline::with_credentials(
            std::sync::Arc::new(pg.clone()),
            surreal,
            std::sync::Arc::new(FixedPushCredentials {
                forgejo_token: "forgejo-token".into(),
                slack_bot_token: slack_bot_token.map(str::to_owned),
            }),
            Some(forgejo.uri()),
            Some(slack.uri()),
        ));

    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    // The pipeline's Surreal handle carries the seeded Project and the
    // recorded channel; the state's own handle serves the receiver,
    // which reads no Project data — only the journal below.
    state.forgejo_webhook_secret = Some(PATH_SECRET.into());
    state.forgejo_webhook_hmac_key = Some(hmac_key.clone());
    state.forgejo_push_store = Some(std::sync::Arc::new(pg.clone()));
    state.forgejo_push_consumer = consumer;
    Some(Fixture {
        app: portal::router(state),
        pg,
        forgejo,
        slack,
        hmac_key,
    })
}

/// POST raw bytes to the receiver with a validly signed body.
async fn post_signed(fixture: &Fixture, body: &[u8]) -> StatusCode {
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(fixture.hmac_key.as_bytes(), body);
    let request = Request::builder()
        .method("POST")
        .uri(format!("/webhook/forgejo/{PATH_SECRET}"))
        .header("content-type", "application/json")
        .header("x-forgejo-signature", signature)
        .body(Body::from(body.to_vec()))
        .unwrap();
    fixture.app.clone().oneshot(request).await.unwrap().status()
}

/// The event key the receiver's normalizations derive for `push_body`'s
/// payload — how a test finds the row the handler journaled.
async fn receipt_for(
    fixture: &Fixture,
    body: &[u8],
) -> forgejo_push_webhook_journal::ForgejoPushReceipt {
    let payload: serde_json::Value = serde_json::from_slice(body).unwrap();
    let ref_updates = serde_json::json!([{
        "ref": payload["ref"],
        "before": payload["before"],
        "after": payload["after"],
    }]);
    let key = forgejo_push_webhook_journal::push_event_key(REPO, &ref_updates);
    forgejo_push_webhook_journal::by_event_key(&fixture.pg, &key)
        .await
        .unwrap()
        .expect("the delivery is journaled")
}

/// The Block Kit message Slack received — the one `chat.postMessage`
/// body this fixture's delivery posts.
async fn delivered_message(server: &MockServer) -> serde_json::Value {
    let requests = server.received_requests().await.unwrap();
    let posted = requests
        .iter()
        .find(|request| request.url.path() == "/chat.postMessage")
        .expect("the delivery posts one message");
    serde_json::from_slice(&posted.body).unwrap()
}

#[tokio::test]
async fn a_verified_push_delivers_one_block_kit_message() {
    let Some(fixture) = fixture(Some("slack-token")).await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "ts": "1700000000.000100", "channel": "C04PUSH"
        })))
        .expect(1)
        .mount(&fixture.slack)
        .await;

    let body = push_body();
    let status = post_signed(&fixture, &body).await;
    assert_eq!(status, StatusCode::OK, "a verified push is consumed");

    let receipt = receipt_for(&fixture, &body).await;
    assert_eq!(receipt.status.as_str(), "delivered");
    assert_eq!(receipt.change_id.as_deref(), Some("qvrpxyq"));
    assert_eq!(receipt.failure_detail, None);

    // The delivered message is the Block Kit render of the analysis, in
    // the Project's (provisioned) channel.
    let message = delivered_message(&fixture.slack).await;
    assert_eq!(message["channel"], "C04PUSH");
    let header = &message["blocks"][0];
    assert_eq!(header["type"], "header");
    assert_eq!(
        header["text"]["text"],
        "Project project-code · change qvrpxyq"
    );
}

#[tokio::test]
async fn an_unconfigured_slack_credential_fails_the_receipt_without_dropping_it() {
    let Some(fixture) = fixture(None).await else {
        return;
    };

    let body = push_body();
    let status = post_signed(&fixture, &body).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a delivery the Firm's credential cannot sign is a 5xx: Forgejo redelivers"
    );

    // The event is not dropped: journaled, analyzed (that part never
    // needed Slack), failed with the reason named, and re-consumable.
    let receipt = receipt_for(&fixture, &body).await;
    assert_eq!(receipt.status.as_str(), "failed");
    assert_eq!(receipt.change_id.as_deref(), Some("qvrpxyq"));
    let detail = receipt.failure_detail.expect("the failure is named");
    assert!(
        detail.contains("Slack bot token"),
        "the error names the missing credential: {detail}"
    );
    assert!(
        !receipt.status.is_terminal(),
        "a failed receipt stays re-consumable"
    );
}

#[tokio::test]
async fn a_redelivery_after_a_delivery_failure_reruns_only_the_unfinished_tail() {
    let Some(fixture) = fixture(Some("slack-token")).await else {
        return;
    };
    // Delivery fails once, then succeeds. The Forgejo reads are mounted
    // with `.expect(1)`: a redelivery must not re-read them.
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .mount(&fixture.slack)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat.postMessage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "ts": "1700000000.000100", "channel": "C04PUSH"
        })))
        .mount(&fixture.slack)
        .await;

    let body = push_body();
    let first = post_signed(&fixture, &body).await;
    assert_eq!(first, StatusCode::INTERNAL_SERVER_ERROR);
    let failed = receipt_for(&fixture, &body).await;
    assert_eq!(failed.status.as_str(), "failed");
    assert_eq!(failed.change_id.as_deref(), Some("qvrpxyq"));

    // Forgejo's redelivery of the same push: the analysis comes off the
    // persisted columns (the `.expect(1)` Forgejo mocks stay satisfied)
    // and only delivery re-runs.
    let second = post_signed(&fixture, &body).await;
    assert_eq!(second, StatusCode::OK);

    let delivered = receipt_for(&fixture, &body).await;
    assert_eq!(
        delivered.id, failed.id,
        "the redelivery dedups to one receipt"
    );
    assert_eq!(delivered.status.as_str(), "delivered");
    assert_eq!(delivered.failure_detail, None);

    let requests = fixture.slack.received_requests().await.unwrap();
    let posts = requests
        .iter()
        .filter(|request| request.url.path() == "/chat.postMessage")
        .count();
    assert_eq!(
        posts, 2,
        "delivery ran once per consumption, not once per phase"
    );
}
