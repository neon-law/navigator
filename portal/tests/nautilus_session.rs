//! The sandbox session, end to end: a prompt walks the spec's session flow
//! through the real gated route family — authenticated caller, subscription
//! check, ledger-fed page state, metered completion with debit-on-success,
//! and the read-only search tools available in-session.
//!
//! Every test drives the same router the deployment mounts, with the
//! deterministic stub provider in the gateway's seat and the search
//! backends deliberately unconfigured (the outage posture). The Purchase
//! ledger is the house fixture's isolated Postgres database, skipped where
//! the environment has none.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use chrono::{Datelike, Utc};
use gateway::StubModelProvider;
use portal::nautilus::{self, SandboxBackends};
use portal::test_support::{app_state, TEST_SESSION_KEY};
use portal::{SessionData, SessionStore};
use store::persons::{NewPerson, Role};
use store::purchases::{Cadence, NewPurchase};
use store::surreal::SurrealDb;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

/// The isolated Postgres database the subscriber-path tests read, mirroring
/// the house fixture: an isolated database off the CI test container, absent
/// locally, so the caller skips the assertions it cannot serve.
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
    let name = format!("nautilus_session_{}", Uuid::new_v4().simple());
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

/// The router, the engines it reads, and the stub provider's handle: the
/// same handle the router holds, so a test scripts and inspects the exact
/// completions the session runs.
struct Fixture {
    app: Router,
    surreal: SurrealDb,
    pg: store::postgres::PostgresDb,
    stub: Arc<StubModelProvider>,
}

/// Skip helper: the fixture, or `None` when this environment has no test
/// Postgres.
async fn fixture() -> Option<Fixture> {
    let pg = database().await?;
    let mut state = app_state(mem_surreal().await).await;
    state.auth = portal::AuthConfig::new(true, None);
    let surreal = state.surreal.clone();
    let stub = Arc::new(StubModelProvider::new());
    let model: Arc<dyn gateway::ModelProvider> = stub.clone();
    let app = nautilus::routes_with(
        &state,
        Some(pg.clone()),
        SandboxBackends {
            model: Some(model),
            // The outage posture: no provider configured, so every search
            // degrades inside the session.
            search: portal_search_backends(),
        },
    )
    .layer(tower_cookies::CookieManagerLayer::new());
    Some(Fixture {
        app,
        surreal,
        pg,
        stub,
    })
}

/// The production-shaped search backends with no provider configured — the
/// way a deployment without CourtListener/Descrybe credentials boots.
fn portal_search_backends() -> mcp::SearchBackends {
    mcp::SearchBackends {
        case_law: None,
        legal_aid: None,
    }
}

/// A subscriber's person row and the session that names it: the real CLI
/// credential carrying the person id and email the OIDC handshake fills in.
async fn seed_subscriber(surreal: &SurrealDb, email: &str) -> (Uuid, String) {
    let person = store::persons::create(
        surreal,
        &NewPerson::with_role("Pat Subscriber", email, Role::Client),
    )
    .await
    .unwrap();
    let mut session = SessionData::fresh(person.email.as_str(), Role::Client);
    session.person_id = Some(person.id);
    session.email = Some(person.email.clone());
    let bearer = format!(
        "Bearer {}",
        SessionStore::new(TEST_SESSION_KEY).encode(&session)
    );
    (person.id, bearer)
}

/// Record the subscriber's Purchase and walk it through Xero's states to
/// `active` — the state the gate reads.
async fn seed_active_purchase(pg: &store::postgres::PostgresDb, person_id: Uuid, email: &str) {
    let purchase = store::purchases::open(
        pg,
        &NewPurchase {
            person_id,
            email: email.to_string(),
            offering: "delete-your-data-monthly".into(),
            offering_name: "delete-your-data-monthly".into(),
            cadence: Cadence::Month,
            amount_cents: 2_900,
            currency: "USD".into(),
            account_code: "200".into(),
        },
        Utc::now(),
    )
    .await
    .unwrap();
    store::purchases::record_invoice(
        pg,
        purchase.id,
        "contact",
        &format!("inv-{}", purchase.id),
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    store::purchases::activate(pg, purchase.id, None, Utc::now())
        .await
        .unwrap();
}

/// Provision the month's row and spend `spent_cents` of it, the way the
/// ledger itself would — `grant` then `debit`.
async fn seed_spent(surreal: &SurrealDb, person_id: Uuid, spent_cents: i64) {
    let period = period();
    store::sandbox_usage::grant(surreal, person_id, &period)
        .await
        .unwrap();
    if spent_cents > 0 {
        store::sandbox_usage::debit(surreal, person_id, &period, spent_cents)
            .await
            .unwrap();
    }
}

/// The billing month the router serves, spelled the way the ledger keys it.
fn period() -> String {
    let now = Utc::now().date_naive();
    store::sandbox_usage::billing_period(now.year(), now.month() as u32)
}

/// The ledger row after a test's traffic.
async fn row(surreal: &SurrealDb, person_id: Uuid) -> store::sandbox_usage::SandboxUsage {
    store::sandbox_usage::find(surreal, person_id, &period())
        .await
        .unwrap()
        .expect("the page provisioned the month's row")
}

async fn get(app: &Router, bearer: &str) -> (StatusCode, String, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(nautilus::NAUTILUS_PATH)
                .header(header::AUTHORIZATION, bearer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    finish(response).await
}

/// The one field the composer posts, form-encoded. The test prompts carry
/// no reserved characters beyond spaces and question marks, so this is the
/// whole encoding the real `application/x-www-form-urlencoded` body needs.
fn form_encode(value: &str) -> String {
    value.replace(' ', "+")
}

/// POST a prompt the way the page's composer does, as a bearer caller —
/// the credential carries no cookie, so the CSRF check has nothing to forge.
async fn post_prompt(app: &Router, bearer: &str, prompt: &str) -> (StatusCode, String, String) {
    let form = format!("prompt={}", form_encode(prompt));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(nautilus::MESSAGES_PATH)
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form))
                .unwrap(),
        )
        .await
        .unwrap();
    finish(response).await
}

async fn finish(response: axum::response::Response) -> (StatusCode, String, String) {
    let status = response.status();
    let location = response
        .headers()
        .get(header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, location, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn a_prompt_runs_one_metered_completion_and_renders_the_conversation() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-happy@example.com").await;
    seed_active_purchase(&fx.pg, person_id, "session-happy@example.com").await;

    // First sight of the page provisions the month's grant.
    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("legal-sandbox-composer"), "{body}");
    assert!(body.contains("$5.00"), "{body}");
    assert!(body.contains("$0.00"), "{body}");
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.granted_cents, 500);
    assert_eq!(usage.spent_cents, 0);

    // One exchange: one completion, one cent (the stub's default price),
    // the reply on the page.
    let (status, location, _) = post_prompt(&fx.app, &bearer, "Is my lease assignable?").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, nautilus::NAUTILUS_PATH);

    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Is my lease assignable?"), "{body}");
    assert!(body.contains("Stub reply 1."), "{body}");
    assert!(body.contains("$0.01"), "{body}");
    assert!(!body.contains("legal-sandbox-error"), "{body}");
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.spent_cents, 1);
    assert!(!usage.overage);

    // The completion carried the session's skills and the two search tools.
    let calls = fx.stub.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        !calls[0].active_skills.is_empty(),
        "the vendored skills load"
    );
    let names: Vec<&str> = calls[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"search_case_law"), "{names:?}");
    assert!(names.contains(&"search_legal_aid"), "{names:?}");
}

#[tokio::test]
async fn an_exhausted_subscriber_gets_the_offer_and_no_completion() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-spent@example.com").await;
    seed_active_purchase(&fx.pg, person_id, "session-spent@example.com").await;
    seed_spent(&fx.surreal, person_id, 500).await;

    // The page shows the à la carte offer, never a composer.
    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("legal-sandbox-exhausted"), "{body}");
    assert!(!body.contains("legal-sandbox-composer"), "{body}");

    // A prompt is refused before any model call: the consent gate.
    let (status, _, _) = post_prompt(&fx.app, &bearer, "One more question?").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        fx.stub.calls().is_empty(),
        "an exhausted session never reaches a model"
    );
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.spent_cents, 500);
}

#[tokio::test]
async fn a_low_balance_subscriber_still_completes_with_the_notice() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-low@example.com").await;
    seed_active_purchase(&fx.pg, person_id, "session-low@example.com").await;
    seed_spent(&fx.surreal, person_id, 490).await;

    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("legal-sandbox-low-balance"), "{body}");
    assert!(body.contains("legal-sandbox-composer"), "{body}");

    // The crossing completion still runs and bills in full — the debit
    // that pushes the row negative flags overage rather than refusing.
    let (status, _, _) = post_prompt(&fx.app, &bearer, "And one more?").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.spent_cents, 491);
    let (_, _, body) = get(&fx.app, &bearer).await;
    assert!(body.contains("Stub reply 1."), "{body}");
}

#[tokio::test]
async fn a_search_outage_degrades_inside_the_session() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-outage@example.com").await;
    seed_active_purchase(&fx.pg, person_id, "session-outage@example.com").await;

    // The model asks for a case-law search; no provider is configured, so
    // the loop hands the degradation back as the tool's result and the
    // session completes anyway.
    fx.stub.queue_tool_calls(vec![gateway::ToolCall {
        id: "call_1".into(),
        name: "search_case_law".into(),
        input: serde_json::json!({"query": "lease assignment"}),
    }]);

    let (status, _, _) = post_prompt(&fx.app, &bearer, "Find cases on lease assignment.").await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("legal-sandbox-turn--tool-call"), "{body}");
    assert!(body.contains("search_case_law"), "{body}");
    assert!(body.contains("legal-sandbox-turn--tool-result"), "{body}");
    assert!(body.contains("unavailable"), "{body}");
    assert!(body.contains("Stub reply 2."), "{body}");
    assert!(!body.contains("legal-sandbox-error"), "{body}");

    // Two completions ran (the tool round, then the answer), both metered.
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.spent_cents, 2);
    let calls = fx.stub.calls();
    assert_eq!(calls.len(), 2);
    // The re-prompt carries the tool's degradation as text.
    let second = &calls[1].messages.last().unwrap().content;
    assert!(second.contains("search_case_law"), "{second}");
    assert!(second.contains("unavailable"), "{second}");
}

#[tokio::test]
async fn a_preview_callers_post_never_reaches_a_model() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-preview@example.com").await;

    let (status, location, _) = post_prompt(&fx.app, &bearer, "Let me in anyway.").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, nautilus::NAUTILUS_PATH);
    assert!(fx.stub.calls().is_empty());
    // No credit row, no transcript: a preview caller spent nothing.
    assert!(
        store::sandbox_usage::find(&fx.surreal, person_id, &period())
            .await
            .unwrap()
            .is_none(),
        "a preview caller is never granted a row"
    );
}

#[tokio::test]
async fn a_failed_completion_charges_nothing_and_flags_the_page() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "session-failure@example.com").await;
    seed_active_purchase(&fx.pg, person_id, "session-failure@example.com").await;
    fx.stub.set_failure(Some("vertex is down".into()));

    let (status, location, _) = post_prompt(&fx.app, &bearer, "Is my lease assignable?").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location,
        format!("{}?error=completion", nautilus::NAUTILUS_PATH)
    );

    // The nothing-charged notice stands, and the saved question shows.
    let (status, _, body) = get(&fx.app, &bearer).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("legal-sandbox-error"), "{body}");
    assert!(body.contains("Is my lease assignable?"), "{body}");
    let usage = row(&fx.surreal, person_id).await;
    assert_eq!(usage.spent_cents, 0, "a failed completion is never charged");
}

#[tokio::test]
async fn the_outbound_request_carries_the_terms_and_no_identity() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let email = "session-sanitized@example.com";
    let (person_id, bearer) = seed_subscriber(&fx.surreal, email).await;
    seed_active_purchase(&fx.pg, person_id, email).await;

    post_prompt(&fx.app, &bearer, "Is my lease assignable?").await;

    // The provider saw the question — and none of the identity the
    // session carries: no email, no person id, no session secret.
    let calls = fx.stub.calls();
    assert_eq!(calls.len(), 1);
    let wire = format!("{:?}", calls[0]);
    assert!(wire.contains("Is my lease assignable?"), "{wire}");
    assert!(!wire.contains(email), "{wire}");
    assert!(!wire.contains(&person_id.to_string()), "{wire}");
    assert!(!wire.contains("csrf"), "{wire}");
}
