//! The Forgejo push webhook receiver, driven end to end through the
//! mounted router: verification refuses before any write, journal first
//! and consume second, redeliveries answer from the journal, and the
//! boot-time replay drains exactly what first delivery left behind.
//!
//! The journal is PostgreSQL-backed, so each test owns an isolated
//! database off the CI test container, absent locally — the caller skips
//! the assertions it cannot serve (the house fixture, as in
//! `nautilus_gate.rs`).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use portal::forgejo_webhook::PushConsumer;
use store::postgres::forgejo_push_webhook_journal::{self, ForgejoPushReceipt, NewPushReceipt};
use tower::ServiceExt;
use uuid::Uuid;

/// The payload Forgejo sends for a jj push of one `push/<change-id>`
/// bookmark, as raw bytes — the exact bytes the HMAC must cover.
fn push_body(ref_name: &str) -> Vec<u8> {
    serde_json::json!({
        "ref": ref_name,
        "before": "0000000000000000000000000000000000000000",
        "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1",
        "total_commits": 1,
        "repository": { "id": 7, "full_name": "firm-slug/project-code" },
        "pusher": { "login": "tbot" }
    })
    .to_string()
    .into_bytes()
}

/// A consumer that records every receipt it is handed and fails its
/// first `failures_left` calls — the script a test gives the pipeline's
/// tail.
struct ScriptedConsumer {
    failures_left: std::sync::Mutex<usize>,
    calls: std::sync::Mutex<Vec<Uuid>>,
}

impl ScriptedConsumer {
    fn with_failures(failures: usize) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            failures_left: std::sync::Mutex::new(failures),
            calls: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Uuid> {
        self.calls.lock().expect("call log").clone()
    }
}

#[async_trait::async_trait]
impl PushConsumer for ScriptedConsumer {
    async fn consume(&self, receipt: &ForgejoPushReceipt) -> Result<(), String> {
        self.calls.lock().expect("call log").push(receipt.id);
        let mut failures_left = self.failures_left.lock().expect("failures");
        if *failures_left > 0 {
            *failures_left -= 1;
            return Err("slack unavailable (scripted)".into());
        }
        Ok(())
    }
}

const HMAC_KEY: &str = "forgejo-receiver-test-key";
const PATH_SECRET: &str = "forgejo-test-secret";

/// The isolated Postgres database the receiver's tests read, mirroring
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
    let name = format!("forgejo_webhook_{}", Uuid::new_v4().simple());
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

struct Fixture {
    app: axum::Router,
    pg: store::postgres::PostgresDb,
    consumer: std::sync::Arc<ScriptedConsumer>,
}

/// A router whose receiver is fully armed: both gates configured, a real
/// journal store, and a scripted consumer.
async fn fixture(failures: usize) -> Option<Fixture> {
    let pg = database().await?;
    let consumer = ScriptedConsumer::with_failures(failures);
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    state.forgejo_webhook_secret = Some(PATH_SECRET.into());
    state.forgejo_webhook_hmac_key = Some(HMAC_KEY.into());
    state.forgejo_push_store = Some(std::sync::Arc::new(pg.clone()));
    state.forgejo_push_consumer = consumer.clone();
    Some(Fixture {
        app: portal::router(state),
        pg,
        consumer,
    })
}

/// POST raw bytes to the receiver with a validly signed body.
async fn post_signed(app: &axum::Router, body: &[u8]) -> StatusCode {
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(HMAC_KEY.as_bytes(), body);
    post_with(app, body, Some(&signature)).await
}

async fn post_with(app: &axum::Router, body: &[u8], signature: Option<&str>) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/webhook/forgejo/{PATH_SECRET}"))
        .header("content-type", "application/json");
    if let Some(signature) = signature {
        request = request.header("x-forgejo-signature", signature);
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap()
        .status()
}

/// Every row in the test's journal — the write-refusal assertions count
/// these, not just the re-consumable ones.
async fn journal_row_count(pg: &store::postgres::PostgresDb) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM forgejo_push_webhook_journal")
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

/// The event key the receiver's normalizations derive for `push_body`'s
/// payload — how a test finds the row the handler journaled.
fn event_key_for(body: &[u8]) -> String {
    let payload: serde_json::Value = serde_json::from_slice(body).unwrap();
    let ref_updates = serde_json::json!([{
        "ref": payload["ref"],
        "before": payload["before"],
        "after": payload["after"],
    }]);
    forgejo_push_webhook_journal::push_event_key("firm-slug/project-code", &ref_updates)
}

#[tokio::test]
async fn a_valid_push_delivery_journals_and_consumes() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");

    let status = post_signed(&fixture.app, &body).await;

    assert_eq!(status, StatusCode::OK);
    let receipt = forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&body))
        .await
        .unwrap()
        .expect("the verified push is journaled");
    assert_eq!(
        receipt.status,
        forgejo_push_webhook_journal::ReceiptStatus::Delivered,
        "journaled first, then consumed inline"
    );
    assert_eq!(fixture.consumer.calls(), vec![receipt.id]);
}

#[tokio::test]
async fn a_tampered_body_is_refused_before_any_write() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let signed = push_body("refs/heads/push/qvrpxyq");
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(HMAC_KEY.as_bytes(), &signed);
    let tampered = push_body("refs/heads/push/evil999");

    let status = post_with(&fixture.app, &tampered, Some(&signature)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        journal_row_count(&fixture.pg).await,
        0,
        "a refusal is not an event — nothing is journaled"
    );
    assert!(fixture.consumer.calls().is_empty());
}

#[tokio::test]
async fn a_delivery_without_a_signature_header_is_refused() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");

    let status = post_with(&fixture.app, &body, None).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(journal_row_count(&fixture.pg).await, 0);
    assert!(fixture.consumer.calls().is_empty());
}

#[tokio::test]
async fn a_delivery_signed_with_the_wrong_key_is_refused() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(b"not-the-key", &body);

    let status = post_with(&fixture.app, &body, Some(&signature)).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(journal_row_count(&fixture.pg).await, 0);
    assert!(fixture.consumer.calls().is_empty());
}

#[tokio::test]
async fn a_path_secret_mismatch_is_refused() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(HMAC_KEY.as_bytes(), &body);

    let status = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhook/forgejo/not-the-secret")
                .header("content-type", "application/json")
                .header("x-forgejo-signature", signature)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(journal_row_count(&fixture.pg).await, 0);
    assert!(fixture.consumer.calls().is_empty());
}

#[tokio::test]
async fn a_push_outside_the_push_namespace_journals_as_skipped() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/main");

    let status = post_signed(&fixture.app, &body).await;

    assert_eq!(status, StatusCode::OK);
    let receipt = forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&body))
        .await
        .unwrap()
        .expect("the verified delivery is journaled even when skipped");
    assert_eq!(
        receipt.status,
        forgejo_push_webhook_journal::ReceiptStatus::Skipped,
        "observable, never analyzed"
    );
    assert!(
        fixture.consumer.calls().is_empty(),
        "a skipped delivery reaches no consumer"
    );
}

#[tokio::test]
async fn a_redelivery_answers_200_and_signals_nothing() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");
    let first = post_signed(&fixture.app, &body).await;
    let redelivery = post_signed(&fixture.app, &body).await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(redelivery, StatusCode::OK, "answered from the journal");
    assert_eq!(
        journal_row_count(&fixture.pg).await,
        1,
        "content-based dedup: one row per push event"
    );
    assert_eq!(
        fixture.consumer.calls().len(),
        1,
        "the pipeline is not signaled twice"
    );
}

#[tokio::test]
async fn a_failing_consumption_answers_5xx_and_recovers_at_replay() {
    let Some(fixture) = fixture(1).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");

    let status = post_signed(&fixture.app, &body).await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the 5xx is what makes Forgejo redeliver"
    );
    let receipt = forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&body))
        .await
        .unwrap()
        .expect("the receipt was journaled before the failed consumption");
    assert_eq!(
        receipt.status,
        forgejo_push_webhook_journal::ReceiptStatus::Failed
    );
    assert!(
        receipt
            .failure_detail
            .as_deref()
            .is_some_and(|d| d.contains("slack unavailable")),
        "the failure is named on the receipt"
    );

    portal::forgejo_webhook::replay_pending_receipts(&fixture_state(&fixture).await, 100)
        .await
        .unwrap();

    let recovered = forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&body))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        recovered.status,
        forgejo_push_webhook_journal::ReceiptStatus::Delivered,
        "the sweep re-ran the unfinished tail"
    );
    assert_eq!(
        recovered.failure_detail, None,
        "a later successful transition clears the failure"
    );
    assert_eq!(
        fixture.consumer.calls().len(),
        2,
        "exactly two consumption attempts: the failed one and the recovery"
    );
}

/// The handler path consumed the receipt already — rebuild the state the
/// sweep reads. The fixture's consumer Arc is shared, so failure-script
/// changes carry over.
async fn fixture_state(fixture: &Fixture) -> portal::AppState {
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    state.forgejo_push_store = Some(std::sync::Arc::new(fixture.pg.clone()));
    state.forgejo_push_consumer = fixture.consumer.clone();
    state
}

/// A verified receipt that outlived its consumption — the crash between
/// journal and consumption — is recovered by the boot-time sweep exactly
/// once, and a receipt already delivered is left alone.
#[tokio::test]
async fn the_boot_sweep_recovers_a_crashed_delivery_exactly_once() {
    let Some(fixture) = fixture(0).await else {
        return;
    };
    let body = push_body("refs/heads/push/qvrpxyq");
    let crash = forgejo_push_webhook_journal::journal_verified_push(
        &fixture.pg,
        &NewPushReceipt {
            delivered_at: chrono::Utc::now(),
            repo_full_name: "firm-slug/project-code".into(),
            ref_updates: serde_json::from_slice(
                &serde_json::to_vec(&serde_json::json!([{
                    "ref": "refs/heads/push/qvrpxyq",
                    "before": "0000000000000000000000000000000000000000",
                    "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1",
                }]))
                .unwrap(),
            )
            .unwrap(),
            payload: String::from_utf8(body.clone()).unwrap(),
        },
    )
    .await
    .unwrap();

    let state = fixture_state(&fixture).await;
    portal::forgejo_webhook::replay_pending_receipts(&state, 100)
        .await
        .unwrap();
    portal::forgejo_webhook::replay_pending_receipts(&state, 100)
        .await
        .unwrap();

    assert_eq!(
        fixture.consumer.calls(),
        vec![crash.id],
        "recovered exactly once; the second sweep re-runs nothing"
    );
    let receipt = forgejo_push_webhook_journal::by_event_key(&fixture.pg, &crash.event_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.status,
        forgejo_push_webhook_journal::ReceiptStatus::Delivered
    );
}

/// The sweep's work list is `pending` and `failed` — a receipt already
/// `delivered` by first delivery is drained by nothing.
#[tokio::test]
async fn the_boot_sweep_reconsumes_pending_and_failed_but_not_delivered() {
    let Some(fixture) = fixture(1).await else {
        return;
    };
    // Failed: the scripted consumer fails its one call, so consumption
    // records its failure. Posted first — the script spends the failure.
    let failed_body = push_body("refs/heads/push/ccccccc");
    post_signed_with_failures(&fixture, &failed_body).await;
    // Delivered by first delivery, through the handler.
    let delivered_body = push_body("refs/heads/push/aaaaaaa");
    post_signed(&fixture.app, &delivered_body).await;
    // Pending: journaled, never consumed.
    let pending_body = push_body("refs/heads/push/bbbbbbb");
    let pending = forgejo_push_webhook_journal::journal_verified_push(
        &fixture.pg,
        &NewPushReceipt {
            delivered_at: chrono::Utc::now(),
            repo_full_name: "firm-slug/project-code".into(),
            ref_updates: serde_json::from_slice(
                &serde_json::to_vec(&serde_json::json!([{
                    "ref": "refs/heads/push/bbbbbbb",
                    "before": "0000000000000000000000000000000000000000",
                    "after": "9f3ba2c7114e0d5689c0b1a2f4e5d6c7b8a9f0e1",
                }]))
                .unwrap(),
            )
            .unwrap(),
            payload: String::from_utf8(pending_body.clone()).unwrap(),
        },
    )
    .await
    .unwrap();

    let state = fixture_state(&fixture).await;
    portal::forgejo_webhook::replay_pending_receipts(&state, 100)
        .await
        .unwrap();

    let calls = fixture.consumer.calls();
    let delivered_receipt =
        forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&delivered_body))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        calls
            .iter()
            .filter(|id| **id == delivered_receipt.id)
            .count(),
        1,
        "first delivery's consumption is never re-run"
    );
    assert!(calls.contains(&pending.id), "pending is re-consumed");
    let failed_receipt =
        forgejo_push_webhook_journal::by_event_key(&fixture.pg, &event_key_for(&failed_body))
            .await
            .unwrap()
            .unwrap();
    assert!(calls.contains(&failed_receipt.id), "failed is re-consumed");
    for id in [delivered_receipt.id, pending.id, failed_receipt.id] {
        let receipt = sqlx::query_scalar::<_, String>(
            "SELECT status FROM forgejo_push_webhook_journal WHERE id = $1",
        )
        .bind(id)
        .fetch_one(fixture.pg.pool())
        .await
        .unwrap();
        assert_eq!(receipt, "delivered", "{id} is drained to terminal");
    }
}

/// Post through the handler with a one-failure consumer, for tests that
/// need a `failed` receipt in the journal.
async fn post_signed_with_failures(fixture: &Fixture, body: &[u8]) {
    // The shared fixture consumer was built with failures; this helper
    // is called on fixtures built with exactly one scripted failure.
    let signature = portal::webhook_auth::sign_hmac_sha256_hex(HMAC_KEY.as_bytes(), body);
    let status = post_with(&fixture.app, body, Some(&signature)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// A verified push when no journal store is configured is refused —
/// never consumed without its receipt, so Forgejo redelivers.
#[tokio::test]
async fn a_verified_push_without_a_journal_store_is_refused() {
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    state.forgejo_webhook_secret = Some(PATH_SECRET.into());
    state.forgejo_webhook_hmac_key = Some(HMAC_KEY.into());
    let app = portal::router(state);
    let body = push_body("refs/heads/push/qvrpxyq");

    let status = post_signed(&app, &body).await;

    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "journal-first means no journal, no consumption"
    );
}
