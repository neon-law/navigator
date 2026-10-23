//! `POST /app/api/seed` with `as_ci`: an operator's dry run under the
//! scoped-session rules a CI-minted session carries, so a scope violation
//! fails on the operator's machine or the pull request rather than on
//! `main`.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;

async fn seed(
    app: &axum::Router,
    bearer: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/app/api/seed")
                .header(header::AUTHORIZATION, bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap_or_default())
}

fn entity_yaml(name: &str) -> String {
    format!(
        "lookup_fields:\n  - name\n  - entity_type_id\nrecords:\n  - name: {name}\n    entity_type:\n      name: LLC\n      jurisdiction:\n        name: Nevada\n"
    )
}

async fn fixture() -> (axum::Router, String, store::surreal::SurrealDb) {
    let surreal = mem_surreal().await;
    store::entity_types::create(&surreal, "LLC").await.unwrap();
    store::jurisdictions::create(
        &surreal,
        &store::jurisdictions::NewJurisdiction::new("Nevada", "NV", "state"),
    )
    .await
    .unwrap();
    store::test_support::seed_project(&surreal, "acme").await;
    let owner = store::persons::create(
        &surreal,
        &NewPerson::with_role("Owner Example", "owner@neonlaw.com", Role::Owner),
    )
    .await
    .unwrap();
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.policy = portal::policy::PolicyClient::embedded().expect("embedded policy compiles");
    let app = portal::router(state);
    let sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    let mut session = portal::SessionData::fresh("owner-sub", Role::Owner);
    session.person_id = Some(owner.id);
    (
        app,
        format!("Bearer {}", sessions.encode(&session)),
        surreal,
    )
}

async fn project_code(surreal: &store::surreal::SurrealDb) -> String {
    store::projects::all(surreal)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("the fixture project")
        .code
}

/// An owner's plain dry run is clean; the same plan under `as_ci` reports
/// the scope violation CI would hit on `main`, in the 403 shape CI gets.
#[tokio::test]
async fn as_ci_applies_the_scoped_rules_an_owner_dry_run_does_not() {
    let (app, bearer, surreal) = fixture().await;
    let code = project_code(&surreal).await;

    let (status, body) = seed(
        &app,
        &bearer,
        serde_json::json!({
            "model": "entity",
            "yaml": entity_yaml("Widget Works LLC"),
            "dry_run": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["created"], 1,
        "an owner's plan says CI would create it"
    );

    let (status, body) = seed(
        &app,
        &bearer,
        serde_json::json!({
            "model": "entity",
            "yaml": entity_yaml("Widget Works LLC"),
            "dry_run": true,
            "as_ci": true,
            "project": code,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "scope_violation");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("scoped session cannot create a new entity"),
        "{body}"
    );

    // The reference model is the door CI does have: the same plan under
    // `as_ci` is clean, and still writes nothing.
    let reference_yaml = "lookup_fields:\n  - project_id\n  - reason\nrecords:\n  - reason: counterparty_entity\n    entity:\n      name: Widget Works LLC\n      entity_type:\n        name: LLC\n        jurisdiction:\n          name: Nevada\n";
    let (status, body) = seed(
        &app,
        &bearer,
        serde_json::json!({
            "model": "project_reference",
            "yaml": reference_yaml,
            "dry_run": true,
            "as_ci": true,
            "project": code,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 1);
    assert!(
        store::entities::find_by_name(&surreal, "Widget Works LLC")
            .await
            .unwrap()
            .is_none(),
        "a dry run writes nothing"
    );
}

/// `as_ci` is a validation capability only, exactly like the pull-request
/// session it stands in for.
#[tokio::test]
async fn as_ci_without_a_plain_dry_run_is_refused() {
    let (app, bearer, surreal) = fixture().await;
    let code = project_code(&surreal).await;
    for body in [
        serde_json::json!({
            "model": "entity", "yaml": entity_yaml("Widget Works LLC"),
            "as_ci": true, "project": code,
        }),
        serde_json::json!({
            "model": "entity", "yaml": entity_yaml("Widget Works LLC"),
            "dry_run": true, "overwrite": true, "as_ci": true, "project": code,
        }),
    ] {
        let (status, answer) = seed(&app, &bearer, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{answer}");
        assert_eq!(answer["error"], "scope_violation");
    }
    let (status, answer) = seed(
        &app,
        &bearer,
        serde_json::json!({
            "model": "entity", "yaml": entity_yaml("Widget Works LLC"),
            "dry_run": true, "as_ci": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{answer}");
    assert_eq!(answer["error"], "invalid_seed");
}
