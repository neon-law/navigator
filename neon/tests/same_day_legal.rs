//! Same Day Legal’s public services and mobile app pages through the mounted router.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use tower::ServiceExt;

async fn app() -> Router {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let host_dioxus = neon::public_dioxus_routers(&state);
    portal::bootstrap(
        state,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
        neon::public_routes(),
        neon::PUBLIC_PATHS,
        host_dioxus,
    )
    .expect("Neon Law public routes must not collide with Navigator")
}

async fn get_on_host(app: &Router, path: &str, host: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(path)
        .header("host", host)
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(request).await.unwrap();
    let status = resp.status();
    let body = String::from_utf8(
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, body)
}

#[tokio::test]
async fn same_day_legal_has_separate_services_and_app_pages() {
    let app = app().await;
    let app_path = views::brand::SAME_DAY_LEGAL_APP_PATH;
    for host in ["sameday.neonlaw.com", "staging-sameday.neonlaw.com"] {
        let (status, home) = get_on_host(&app, "/", host).await;
        assert_eq!(status, StatusCode::OK, "{host}: {home}");
        for expected in [
            "<title>Same Day Legal | Home</title>",
            "fm-page--same-day-legal",
            "What is your legal need?",
            "Start an LLC",
            r#"href="/app""#,
            "Email Same Day Legal",
        ] {
            assert!(home.contains(expected), "{host} is missing {expected:?}");
        }
        assert!(!home.contains("data-enrollment-intake"));
        let (status, _) = get_on_host(&app, "/app/projects", host).await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let (status, body) = get_on_host(&app, app_path, host).await;
        assert_eq!(status, StatusCode::OK, "{host}: {body}");
        for expected in [
            "<title>Same Day Legal | App</title>",
            "fm-page--same-day-legal",
            r#"data-phone="ios""#,
            r#"data-store="ios""#,
            r#"data-store="android""#,
            "Identity theft insurance",
            "Dark web monitoring",
            "Vesta Estate Planning",
            "$29/mo",
            "$290/year",
            "$50",
            "forming an LLC",
            "Lex Tecnica, Ltd.",
            r##"href="#enrollment""##,
            r#"action="/leads""#,
            r#"data-enrollment-intake="true""#,
            "Send and continue to sign in",
            "Attorney advertisement.",
            "site-footer",
        ] {
            assert!(body.contains(expected), "{host} is missing {expected:?}");
        }
        for retired in ["Nautilus", "From $20", "non-legal service"] {
            assert!(!body.contains(retired), "{host} still carries {retired:?}");
        }
    }
    let (status, _) = get_on_host(&app, app_path, "www.neonlaw.com").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
