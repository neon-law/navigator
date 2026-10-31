//! Integration tests for the Owner-only move-brand door:
//! `POST /app/owner/firms/move-brand` — the portal route over
//! `store::firms::move_brand` (PR #206).
//!
//! These prove at the HTTP boundary what the store's own tests cannot:
//!
//! - **The Owner's move lands and reports the new attribution.** A `303` back
//!   to the control with `?moved=neon&firm=<target name>`, and the brand
//!   actually wears the target Firm afterwards.
//! - **Only the Owner gets through.** An Admin session receives the
//!   committed `404` the handler returns (Rego admits the session, the
//!   handler denies the tier — defense in depth), a request with no session
//!   at all never reaches the handler: the policy layer bounces it with a
//!   `303` to `/auth/login?return_to=…` ("click protected link → land at the
//!   IdP"), and the brand is untouched either way.
//! - **A refusal redirects back with the rule and both pickers echoed**, so
//!   nothing is retyped, and the brand is untouched.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "portal-move-brand-route-test-key";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    owner: String,
    admin: String,
    target_firm_id: Uuid,
}

fn bearer(person_id: Uuid, role: Role) -> String {
    let mut session = SessionData::fresh("portal-move-brand-route-test-sub", role);
    session.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&session))
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let owner_id = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Owner",
            format!("owner-{}@example.com", Uuid::now_v7()),
            Role::Owner,
        ),
    )
    .await
    .unwrap()
    .id;
    let admin_id = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Admin",
            format!("admin-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap()
    .id;

    // The house brand `neon` seeds as a registered row under `mem_surreal`;
    // `seed_firm_with_brand` gives it a Firm to wear, and the target is a
    // second Firm for it to move to.
    seed_firm_with_brand(&surreal, "neon").await;
    let target = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Lex Tecnica LLP".into(),
            status: "active".into(),
            entity_id: seed_entity(&surreal).await,
            admin_dri_person_id: admin_id,
        },
    )
    .await
    .unwrap();

    let state = AppState {
        sessions: SessionStore::new(KEY),
        ..portal::test_support::app_state(surreal.clone()).await
    };

    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        owner: bearer(owner_id, Role::Owner),
        admin: bearer(admin_id, Role::Admin),
        target_firm_id: target.id,
    }
}

async fn post_move(
    app: &axum::Router,
    token: Option<&str>,
    body: &str,
) -> (StatusCode, String, String) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/app/owner/firms/move-brand")
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(token) = token {
        builder = builder.header("authorization", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        location,
        String::from_utf8_lossy(&bytes).to_string(),
    )
}

async fn brand_firm_id(surreal: &store::surreal::SurrealDb) -> Option<Uuid> {
    store::brands::find_by_key(surreal, "neon")
        .await
        .unwrap()
        .expect("the neon brand seeds")
        .firm_id
}

fn move_body(target_firm_id: Uuid) -> String {
    format!("brand_key=neon&target_firm_id={target_firm_id}&confirm=on")
}

#[tokio::test]
async fn the_owner_moves_a_brand_and_the_move_reports_the_new_attribution() {
    let fx = build_fixture().await;
    let (status, location, body) =
        post_move(&fx.app, Some(&fx.owner), &move_body(fx.target_firm_id)).await;

    assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
    assert!(
        location.starts_with("/app/owner/firms/move-brand?"),
        "{location}"
    );
    assert!(location.contains("moved=neon"), "{location}");
    // The new attribution the brand's public pages will render.
    assert!(location.contains("firm=Lex"), "{location}");

    // The brand actually wears the target Firm now.
    assert_eq!(
        brand_firm_id(&fx.surreal).await,
        Some(fx.target_firm_id),
        "the brand should wear the target firm after the move"
    );
}

#[tokio::test]
async fn a_non_owner_and_an_anonymous_session_get_the_committed_not_found() {
    let fx = build_fixture().await;
    let body = move_body(fx.target_firm_id);
    let before = brand_firm_id(&fx.surreal).await;

    // The Admin session clears the policy layer (it authenticates) and
    // meets the handler's own committed not-found.
    let (status, _location, response_body) = post_move(&fx.app, Some(&fx.admin), &body).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response_body}");

    // With no session at all the policy layer bounces to the IdP start
    // before any handler runs — the "land at the IdP" experience.
    let (status, location, response_body) = post_move(&fx.app, None, &body).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "{response_body}");
    assert!(
        location.starts_with("/auth/login?return_to=/app/owner/firms/move-brand"),
        "{location}"
    );

    // Neither attempt moved anything.
    assert_eq!(
        brand_firm_id(&fx.surreal).await,
        before,
        "a refused session must not move the brand"
    );
}

#[tokio::test]
async fn a_refusal_redirects_back_with_the_rule_and_both_pickers_echoed() {
    let fx = build_fixture().await;
    let before = brand_firm_id(&fx.surreal).await;
    // The confirm checkbox left unchecked — the first gate the handler holds.
    let body = format!("brand_key=neon&target_firm_id={}", fx.target_firm_id);
    let (status, location, response_body) = post_move(&fx.app, Some(&fx.owner), &body).await;

    assert_eq!(status, StatusCode::SEE_OTHER, "{response_body}");
    assert!(location.contains("error=Confirm%20the%20move"), "{location}");
    assert!(location.contains("brand_key=neon"), "{location}");
    assert!(
        location.contains(&format!("target_firm_id={}", fx.target_firm_id)),
        "{location}"
    );

    // The refused move changed nothing.
    assert_eq!(brand_firm_id(&fx.surreal).await, before);
}

