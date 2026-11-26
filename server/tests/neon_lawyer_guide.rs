//! Neon Law's public lawyer guide and contact redirect.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use tower::ServiceExt;
use views::brand::BrandKey;

async fn site_app() -> Router {
    let mut state =
        portal::test_support::app_state(store::test_support::uninitialized_store()).await;
    state.canonical_host = portal::CanonicalHost::new(None)
        .with_local_ports([(32100, BrandKey::Neon), (32101, BrandKey::GuiebShook)].into());
    server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR))
}

async fn get(app: &Router, path: &str, host: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_string(response: Response) -> String {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

#[tokio::test]
async fn neon_contact_redirects_to_guieb_shook() {
    let app = site_app().await;
    let response = get(&app, "/contact", "localhost:32100").await;
    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        response.headers()["location"],
        "https://guiebshook.com/contact"
    );

    let contact = get(&app, "/contact", "localhost:32101").await;
    assert_eq!(contact.status(), StatusCode::OK);
    assert!(body_string(contact).await.contains(r#"id="cf-need""#));
}

#[tokio::test]
async fn lawyer_guide_offers_self_help_and_representation() {
    let app = site_app().await;
    let home = get(&app, "/", "localhost:32100").await;
    assert_eq!(home.status(), StatusCode::OK);
    let home = body_string(home).await;
    assert!(home.contains("Agentic lawyering is for everyone."));
    assert!(home.contains(r#"href="/do-i-need-a-lawyer""#));
    assert!(!home.contains(r#"href="/legal-aid""#));

    let response = get(&app, "/do-i-need-a-lawyer", "localhost:32100").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(response).await;
    for expected in [
        "Do I need a lawyer?",
        "When you can start on your own",
        "When to talk to a lawyer",
        "Check whether representation is required",
        "valid retainer",
        r#"href="/notations""#,
        r#"href="https://guiebshook.com/contact""#,
    ] {
        assert!(body.contains(expected), "missing {expected}");
    }

    let sibling = get(&app, "/do-i-need-a-lawyer", "localhost:32101").await;
    assert_eq!(sibling.status(), StatusCode::NOT_FOUND);
}
