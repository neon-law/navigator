//! The `/app/nautilus` gate: who reaches the Legal Sandbox.
//!
//! Four outcomes, each against the real gated route family:
//!
//! 1. An anonymous browser is sent through the login door — `303` to
//!    `/auth/login?return_to=/app/nautilus`.
//! 2. An authenticated caller without an active subscription sees the
//!    preview state, never a composer.
//! 3. An authenticated caller whose Purchase of a Nautilus offering is
//!    `active` sees the composer placeholder (the Dioxus sandbox page is a
//!    sibling change; the gate and the extension it reads are its contract).
//! 4. The `OIDC_DISABLED` dev posture keeps working: with no verifier
//!    configured, a signed session blob still reaches the gate, and an
//!    enforced deployment's raw OIDC bearer JWT resolves to the same
//!    subscription check through the existing `require_session` path.
//!
//! The subscriber-path tests read a real Purchase ledger. They follow the
//! house fixture: an isolated database off the CI test container
//! (`NAVIGATOR_TEST_POSTGRES_PORT`), skipped where it is absent.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use chrono::Utc;
use portal::test_support::{app_state, TEST_SESSION_KEY};
use portal::{nautilus, SessionData, SessionStore};
use store::persons::{NewPerson, Role};
use store::purchases::{self, Cadence, NewPurchase};
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
    let name = format!("nautilus_gate_{}", Uuid::new_v4().simple());
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

/// A router and the engine it reads: every person a test seeds goes into
/// `surreal` — the same handle the router's gate consults — so a seeded
/// subscriber is never invisible to the router it drives.
struct Fixture {
    app: Router,
    surreal: SurrealDb,
    pg: store::postgres::PostgresDb,
}

/// Skip helper for the pg-backed outcomes: returns the fixture, or `None`
/// when this environment has no test Postgres.
async fn fixture() -> Option<Fixture> {
    let pg = database().await?;
    Some(fixture_with(pg, portal::AuthConfig::new(true, None)).await)
}

/// [`fixture`] under a chosen auth posture.
async fn fixture_with(pg: store::postgres::PostgresDb, auth: portal::AuthConfig) -> Fixture {
    let mut state = app_state(mem_surreal().await).await;
    state.auth = auth;
    let surreal = state.surreal.clone();
    // Bootstrap applies the cookie manager around the whole application; the
    // session boundary reads cookies through it. Mirror that layering here.
    let app = nautilus::routes_with(&state, Some(pg.clone()))
        .layer(tower_cookies::CookieManagerLayer::new());
    Fixture { app, surreal, pg }
}

/// A subscriber's person row and the session that names it: the real CLI
/// credential — the same signed session blob the browser holds as a cookie,
/// presented as a bearer header — carrying the person id and email the OIDC
/// handshake would fill in.
async fn seed_subscriber(surreal: &SurrealDb, email: &str) -> (Uuid, String) {
    let person = store::persons::create(
        surreal,
        &NewPerson::with_role("Pat Subscriber", email, Role::Client),
    )
    .await
    .unwrap();
    let mut session = SessionData::fresh(person.email.as_str(), Role::Client);
    session.person_id = Some(person.id);
    // `fresh` seeds the OIDC subject only; the email rides the session the
    // way the real OIDC handshake fills it.
    session.email = Some(person.email.clone());
    let bearer = format!(
        "Bearer {}",
        SessionStore::new(TEST_SESSION_KEY).encode(&session)
    );
    (person.id, bearer)
}

/// Record the subscriber's Purchase of `offering` and walk it through Xero's
/// states: `open` records it `invoicing`, `record_invoice` turns it
/// `invoiced`, `activate` turns it `active` — the state the gate reads.
async fn seed_active_purchase(
    pg: &store::postgres::PostgresDb,
    person_id: Uuid,
    email: &str,
    offering: &str,
    amount_cents: i64,
    cadence: Cadence,
) {
    let purchase = open_purchase(pg, person_id, email, offering, amount_cents, cadence).await;
    purchases::record_invoice(
        pg,
        purchase.id,
        "contact",
        &format!("inv-{}", purchase.id),
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    purchases::activate(pg, purchase.id, None, Utc::now())
        .await
        .unwrap();
}

/// Record the Purchase and stop where Xero stops before it reports PAID:
/// `invoicing`, then `invoiced` once the invoice is raised.
async fn open_purchase(
    pg: &store::postgres::PostgresDb,
    person_id: Uuid,
    email: &str,
    offering: &str,
    amount_cents: i64,
    cadence: Cadence,
) -> purchases::Purchase {
    purchases::open(
        pg,
        &NewPurchase {
            person_id,
            email: email.to_string(),
            offering: offering.into(),
            offering_name: offering.into(),
            cadence,
            amount_cents,
            currency: "USD".into(),
            account_code: "200".into(),
        },
        Utc::now(),
    )
    .await
    .unwrap()
}

async fn get(app: &Router, bearer: Option<&str>) -> (StatusCode, String, String) {
    let mut builder = Request::builder().uri(nautilus::NAUTILUS_PATH);
    if let Some(token) = bearer {
        builder = builder.header(header::AUTHORIZATION, token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
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
async fn an_anonymous_browser_is_sent_through_the_login_door() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (status, location, _) = get(&fx.app, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location, "/auth/login?return_to=/app/nautilus");
}

#[tokio::test]
async fn a_caller_without_a_subscription_sees_the_preview() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    // Default test state runs the `OIDC_DISABLED` posture: no verifier, the
    // signed session blob is the whole credential — the dev path end to end.
    let (_, bearer) = seed_subscriber(&fx.surreal, "gate-unsubscribed@example.com").await;
    let (status, _, body) = get(&fx.app, Some(&bearer)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("data-sandbox=\"preview\""),
        "an unsubscribed caller must see the preview, got: {body}"
    );
}

#[tokio::test]
async fn an_unpaid_subscription_still_previews() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "gate-unpaid@example.com").await;
    // Recorded but never `active`: Xero has not reported the invoice PAID.
    let purchase = open_purchase(
        &fx.pg,
        person_id,
        "gate-unpaid@example.com",
        "delete-your-data-monthly",
        2_900,
        Cadence::Month,
    )
    .await;
    assert_eq!(purchase.state, purchases::PurchaseState::Invoicing);
    let (status, _, body) = get(&fx.app, Some(&bearer)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("data-sandbox=\"preview\""),
        "an unactivated subscription must still preview, got: {body}"
    );
}

#[tokio::test]
async fn an_active_subscriber_sees_the_composer() {
    let Some(fx) = fixture().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let (person_id, bearer) = seed_subscriber(&fx.surreal, "gate-subscriber@example.com").await;
    seed_active_purchase(
        &fx.pg,
        person_id,
        "gate-subscriber@example.com",
        "delete-your-data-monthly",
        2_900,
        Cadence::Month,
    )
    .await;
    let (status, _, body) = get(&fx.app, Some(&bearer)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("data-sandbox=\"composer\""),
        "an active subscriber must reach the composer, got: {body}"
    );
}

#[tokio::test]
async fn an_enforced_deployment_admits_a_raw_oidc_bearer_jwt() {
    const SECRET: &str = "gate-secret";
    const SUBJECT: &str = "idp-subject-42";
    let Some(pg) = database().await else {
        eprintln!("skipping: no NAVIGATOR_TEST_POSTGRES_PORT");
        return;
    };
    let fx = fixture_with(pg, portal::AuthConfig::new(false, Some(SECRET))).await;
    let person = store::persons::create(
        &fx.surreal,
        &NewPerson {
            oidc_subject: Some(SUBJECT.into()),
            ..NewPerson::with_role("Jwt Subscriber", "gate-jwt@example.com", Role::Client)
        },
    )
    .await
    .unwrap();
    seed_active_purchase(
        &fx.pg,
        person.id,
        "gate-jwt@example.com",
        "delete-your-data-annual",
        29_000,
        Cadence::Year,
    )
    .await;

    let claims = portal::AuthClaims {
        sub: SUBJECT.into(),
        exp: i64::try_from(jsonwebtoken::get_current_timestamp() + 3_600).unwrap(),
        role: Role::Client,
    };
    let jwt = jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap();

    let (status, _, body) = get(&fx.app, Some(&format!("Bearer {jwt}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("data-sandbox=\"composer\""),
        "a bearer-JWT subscriber must reach the composer, got: {body}"
    );
}
