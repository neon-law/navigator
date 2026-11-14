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

async fn state() -> portal::AppState {
    portal::test_support::app_state(store::test_support::mem_surreal().await).await
}

/// The same state, but for a deployment whose own configured host is
/// `deployment_host` — see `server/tests/delete_your_debt_gateway.rs` for why
/// this is set on `AppState::canonical_host` rather than the per-request
/// `Host:` header.
async fn state_for_deployment(deployment_host: &str) -> portal::AppState {
    let mut state = state().await;
    state.canonical_host = portal::CanonicalHost::new(Some(deployment_host.to_string()));
    state
}

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

/// `GET /delete-your-data` on a Neon host returns 200, wears Neon chrome, and
/// carries the approved original copy — the destination named factually, the
/// clarifier that not every trace can be promised removed, and a
/// destination-named CTA whose href resolves to this deployment's own
/// DeleteYourData.com host.
#[tokio::test]
async fn the_gateway_renders_on_neon_hosts_with_neon_chrome_and_the_approved_copy() {
    let app = app(state().await);

    for host in [None, Some("www.neonlaw.com")] {
        let (status, body) = get_on_host(&app, "/delete-your-data", host).await;
        assert_eq!(status, StatusCode::OK, "{host:?}: {body}");
        for expected in [
            "<title>Neon Law | Delete Your Data</title>",
            r#"content="Neon Law | Privacy and data removal" property="og:title""#,
            "Make your personal information harder to find.",
            "DeleteYourData.com",
            "Guieb &#38; Shook PC",
            "does not promise",
            "Visit DeleteYourData.com",
            "site-footer",
            "/public/css/brand-neon-tokens.css",
        ] {
            assert!(body.contains(expected), "{host:?} is missing {expected:?}");
        }
        assert!(
            body.contains(r#"href="https://www.deleteyourdata.com""#),
            "{host:?}: the CTA does not open the production DeleteYourData.com destination: {body}"
        );
    }
}

/// A staging deployment resolves the CTA to the staging sibling.
#[tokio::test]
async fn a_staging_deployment_resolves_the_cta_to_the_staging_sibling() {
    let app = app(state_for_deployment("staging.neonlaw.com").await);
    let (status, body) = get_on_host(&app, "/delete-your-data", Some("www.neonlaw.com")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains(r#"href="https://staging.deleteyourdata.com""#),
        "the CTA must resolve to the staging sibling on a staging deployment: {body}"
    );
}

/// The same path is a `404` on every other registered brand host — including
/// DeleteYourData.com's own — because only Neon's `publishes_firm_path`
/// admits it.
#[tokio::test]
async fn the_gateway_is_404_on_every_non_neon_brand_host() {
    let app = app(state().await);
    for host in [
        "www.deleteyourdata.com",
        "staging.deleteyourdata.com",
        "www.guiebshook.com",
        "www.vestaestateplanning.com",
        "www.misericordialaw.com",
        "www.abhayaimmigration.com",
        "www.deleteyourdebt.com",
        "www.summonsdefense.nyc",
        "www.daybridgedivorce.com",
    ] {
        let (status, body) = get_on_host(&app, "/delete-your-data", Some(host)).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{host} must not serve the Neon-only gateway: {body}"
        );
    }
}

/// Same Day Legal's home on its own hosts describes the subscription: the
/// checks it runs, drawn as one iPhone app listing, monthly and prepaid annual
/// pricing, the free tool and the single attorney review, and the intake.
#[tokio::test]
async fn the_same_day_legal_home_renders_with_the_app_checks_and_pricing() {
    let app = app(state().await);
    for host in ["sameday.neonlaw.com", "staging-sameday.neonlaw.com"] {
        let (status, body) = get_on_host(&app, "/", Some(host)).await;
        assert_eq!(status, StatusCode::OK, "{host}: {body}");
        for expected in [
            "<title>Same Day Legal | Home</title>",
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
}

/// Neon's `/same-day-legal` link and retired `/nautilus` page answer a 301 to
/// Same Day Legal's host; every other brand host 404s both.
#[tokio::test]
async fn neon_redirects_to_same_day_legal_and_other_brand_hosts_404() {
    let app = app(state().await);
    for path in ["/same-day-legal", "/nautilus"] {
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri(path)
                    .header("host", "www.neonlaw.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY, "{path}");
        assert_eq!(
            response.headers()["location"],
            "https://sameday.neonlaw.com",
            "{path}"
        );
        for host in [
            "www.deleteyourdata.com",
            "www.vestaestateplanning.com",
            "www.cyberinjurylaw.com",
            "www.guiebshook.com",
            "sameday.neonlaw.com",
        ] {
            let (status, _) = get_on_host(&app, path, Some(host)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{host}{path}");
        }
    }
}
