//! Router-level tests for the main-push notice door: `POST
//! /app/api/projects/{id}/events/main-push` admits only the document-scoped
//! CI session minted for that exact Project.

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use portal::github_oidc::{GitHubActionsClaims, GitHubOidc};
use portal::test_support::lawyer_bearer_header;
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    project_id: Uuid,
    other_project_id: Uuid,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "acme".into(),
            name: "Acme".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    let other_project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "widgets".into(),
            name: "Widgets".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    store::projects::set_repository_url(
        &surreal,
        project_id,
        Some("https://github.com/neon-law-staging/acme"),
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Lawyer", "ci-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    for project in [project_id, other_project_id] {
        store::projects::designate_dri_in_surreal(
            &surreal,
            project,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
    }

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
    state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
        sub: "repo:neon-law-staging/acme:ref:refs/heads/main".into(),
        repository: "neon-law-staging/acme".into(),
        repository_owner: "neon-law-staging".into(),
        git_ref: "refs/heads/main".into(),
        event_name: "push".into(),
        jti: "jti-main-push-acme".into(),
        exp: 4_000_000_000,
        ..Default::default()
    });

    Fixture {
        app: portal::router(state),
        project_id,
        other_project_id,
    }
}

async fn json_response(response: Response<Body>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&body).unwrap_or_else(
        |_| serde_json::json!({"body": String::from_utf8_lossy(&body).to_string()}),
    );
    (status, json)
}

async fn mint_document_token(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/ci/document-token")
                .header(header::HOST, "staging.neonlaw.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"token":"github-jwt"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, minted) = json_response(response).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    minted["token"].as_str().unwrap().to_string()
}

fn event() -> serde_json::Value {
    serde_json::json!({
        "short_sha": "abc1234",
        "run_url": "https://github.com/neon-law-staging/acme/actions/runs/1/attempts/1",
        "author_login": "octocat",
        "gate": "pass",
        "run_attempt": 1
    })
}

async fn post_event(
    app: &Router,
    project_id: Uuid,
    authorization: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/app/api/projects/{project_id}/events/main-push"))
        .header(header::HOST, "staging.neonlaw.com")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    json_response(response).await
}

#[tokio::test]
async fn the_door_answers_401_without_a_session() {
    let fixture = fixture().await;
    let (status, _) = post_event(&fixture.app, fixture.project_id, None, event()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_door_answers_403_for_the_wrong_project_and_for_a_session_without_the_ci_scope() {
    let fixture = fixture().await;
    let token = mint_document_token(&fixture.app).await;
    let bearer = format!("Bearer {token}");

    let (status, body) = post_event(
        &fixture.app,
        fixture.other_project_id,
        Some(&bearer),
        event(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "scope_violation");

    // A lawyer's own CLI session is lawyer tier at the policy but carries no
    // CI scope, so the handler refuses it: this door is CI's alone.
    let lawyer = lawyer_bearer_header();
    let (status, body) = post_event(&fixture.app, fixture.project_id, Some(&lawyer), event()).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn the_scoped_session_reaches_its_own_project_and_a_malformed_event_is_400() {
    let fixture = fixture().await;
    let token = mint_document_token(&fixture.app).await;
    let bearer = format!("Bearer {token}");

    // No Restate ingress is configured for a test router, so the door that
    // has admitted and validated the event reports the missing ingress —
    // the step after authorization, and nothing in the matter was touched.
    let (status, body) = post_event(&fixture.app, fixture.project_id, Some(&bearer), event()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"], "workflows_unavailable");

    let mut malformed = event();
    malformed["short_sha"] = serde_json::Value::String("not-hex".into());
    let (status, body) =
        post_event(&fixture.app, fixture.project_id, Some(&bearer), malformed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "invalid_event");

    let mut with_subject = event();
    with_subject["subject"] = serde_json::Value::String("feat: a commit subject".into());
    let (status, _) = post_event(
        &fixture.app,
        fixture.project_id,
        Some(&bearer),
        with_subject,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a commit subject is not a field this door accepts"
    );
}
