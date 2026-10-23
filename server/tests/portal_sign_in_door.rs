//! `GET /auth/login` as a Project portal's front door.
//!
//! A deployment with one identity provider and no password door redirects a
//! signed-out visitor straight to the provider. A visitor who arrived from a
//! portal link instead meets a page of ours first: it names the firm and the
//! matter from the link's `return_to`, says access is by invitation, and
//! offers Contact us beside the provider button. Every other sign-in keeps
//! the immediate redirect, so a firm-side deep link stays one click shorter.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::oauth::OAuthConfig;
use portal::{AppState, AuthConfig};
use store::test_support::mem_surreal;
use tower::ServiceExt;

const AUTHORIZE_ENDPOINT: &str = "https://idp.example.com/oauth/authorize";

/// One provider, no password door: the shape that used to redirect every
/// visitor without rendering anything of ours.
async fn app() -> (axum::Router, store::surreal::SurrealDb) {
    let surreal = mem_surreal().await;
    let state = AppState {
        auth: AuthConfig::new(false, None),
        oauth: Some(OAuthConfig::new(
            "navigator-web",
            "secret",
            "https://app.example.com/auth/callback",
            AUTHORIZE_ENDPOINT,
            "https://idp.example.com/oauth/token",
        )),
        ..portal::test_support::app_state(surreal.clone()).await
    };
    (
        server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
    )
}

async fn get_login(app: &axum::Router, uri: &str) -> (StatusCode, Option<String>, String) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let location = response
        .headers()
        .get("location")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        location,
        String::from_utf8_lossy(&body).into_owned(),
    )
}

#[tokio::test]
async fn a_portal_destination_renders_the_branded_door_naming_the_project() {
    let (app, surreal) = app().await;
    let project =
        store::test_support::seed_project(&surreal, "Estate of Cornelius Montgomery").await;
    let return_to = format!("/app/projects/{}/portal/", project.code);

    let (status, location, html) =
        get_login(&app, &format!("/auth/login?return_to={return_to}")).await;
    assert_eq!(status, StatusCode::OK, "{location:?}");
    assert_eq!(location, None, "no redirect: the page is ours");

    let firm = webapp::public_chrome::firm_public_chrome(vec![]).firm_name;
    for needle in [
        "<h1>Portal</h1>",
        firm.as_str(),
        "Estate of Cornelius Montgomery",
        "Access is by invitation.",
        webapp::auth_pages::CONTACT_US,
        "href=\"/contact\"",
        webapp::auth_pages::GOOGLE_SIGN_IN,
        &format!("/auth/login/oidc?return_to={return_to}"),
    ] {
        assert!(html.contains(needle), "missing {needle}: {html}");
    }
    assert!(
        !html.contains(AUTHORIZE_ENDPOINT),
        "the provider is a button, not the page: {html}"
    );
}

#[tokio::test]
async fn a_portal_destination_with_no_live_project_still_renders_the_firm_copy() {
    let (app, _surreal) = app().await;
    let (status, _, html) = get_login(
        &app,
        "/auth/login?return_to=/app/projects/no-such-code/portal/",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("<h1>Portal</h1>"), "{html}");
    assert!(!html.contains(" Portal for "), "no matter named: {html}");
    assert!(html.contains(webapp::auth_pages::CONTACT_US), "{html}");
}

#[tokio::test]
async fn a_non_portal_destination_keeps_the_immediate_provider_redirect() {
    let (app, surreal) = app().await;
    let project = store::test_support::seed_project(&surreal, "Workbench matter").await;
    for return_to in [
        format!("/app/projects/{}", project.code),
        format!("/app/projects/{}/people/new", project.code),
        "/app/team".to_string(),
        "/app/admin/entities".to_string(),
    ] {
        let (status, location, _) =
            get_login(&app, &format!("/auth/login?return_to={return_to}")).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "{return_to}");
        let location = location.expect("a redirect to the provider");
        assert!(
            location.starts_with(AUTHORIZE_ENDPOINT),
            "{return_to} must redirect to the provider, got {location}"
        );
    }
}

#[tokio::test]
async fn a_missing_destination_keeps_the_immediate_provider_redirect() {
    let (app, _surreal) = app().await;
    let (status, location, _) = get_login(&app, "/auth/login").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        location
            .as_deref()
            .is_some_and(|location| location.starts_with(AUTHORIZE_ENDPOINT)),
        "{location:?}"
    );
}
