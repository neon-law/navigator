//! `/delete-your-data` — Neon's own gateway page to DeleteYourData.com.
//!
//! Neon-only: the same path 404s on every other registered brand host, the
//! same mechanism that already gates `/delete-your-debt`, `/business`,
//! `/services`, and `/disputes` off a house-brand host. See
//! `neon::firm_copy::delete_your_data_gateway` for the content and
//! `views::brand::BrandKey::publishes_firm_path` for the gate.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use tower::ServiceExt;

fn app(state: portal::AppState) -> Router {
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

async fn get_on_host(app: &Router, path: &str, host: Option<&str>) -> (StatusCode, String) {
    let mut builder = Request::builder().uri(path);
    if let Some(host) = host {
        builder = builder.header("host", host);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
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
async fn same_day_legal_home_offers_attorney_options() {
    store::test_support::with_postgres(check_same_day_home()).await;
}

async fn check_same_day_home() {
    let mut state = portal::test_support::app_state(
        store::application_store::ApplicationStore::uninitialized(),
    )
    .await;
    state.oauth = Some(portal::OAuthConfig::new(
        "client",
        "secret",
        "https://primary.example/auth/callback",
        "https://idp.example/authorize",
        "https://idp.example/token",
    ));
    store::seed::seed_canonical(&state.application_store, &state.assets_storage)
        .await
        .expect("seed the public brands");
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    let app = app(state).layer(axum::middleware::from_fn(
        move |mut request: axum::extract::Request, next: axum::middleware::Next| {
            request.extensions_mut().insert(pg.clone());
            next.run(request)
        },
    ));
    let app_path = views::brand::SAME_DAY_LEGAL_APP_PATH;
    for host in ["sameday.neonlaw.com", "staging-sameday.neonlaw.com"] {
        let (status, home) = get_on_host(&app, "/", Some(host)).await;
        assert_eq!(status, StatusCode::OK, "{host}: {home}");
        for expected in [
            "<title>Same Day Legal | Home</title>",
            "fm-page--same-day-legal",
            r#"id="attorney-options""#,
            r#"href="/app#pricing""#,
            "From $50",
            r#"href="mailto:"#,
        ] {
            assert!(
                home.contains(expected),
                "{host} is missing {expected:?}: {home}"
            );
        }
        let html = scraper::Html::parse_document(&home);
        let removed = scraper::Selector::parse(
            ".fm-hero a[href='/auth/signup'], .fm-hero a[href^='tel:'], #mcp-setup, a[href='/#mcp-setup']",
        )
        .unwrap();
        assert!(html.select(&removed).next().is_none(), "{host}");
        let (status, signup) = get_on_host(&app, "/auth/signup", Some(host)).await;
        assert_eq!(status, StatusCode::OK, "{host}: {signup}");
        assert!(signup.contains(r#"action="/auth/signup/age""#), "{host}");
        assert!(!home.contains("data-enrollment-intake"));
        let (status, _) = get_on_host(&app, "/app/projects", Some(host)).await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let (status, body) = get_on_host(&app, app_path, Some(host)).await;
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
            "From $50",
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
    let (status, _) = get_on_host(&app, app_path, Some("www.neonlaw.com")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
