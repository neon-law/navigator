//! The Vesta `/world` calculated astrocartography page through the shipped
//! router: served on Vesta hosts, 404 on every other brand's. The page's
//! content, form, and privacy invariants are covered by `webapp`'s render
//! tests; this file proves the routing and the injected content end to end.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use portal::{AppState, CanonicalHost};
use tower::ServiceExt;
use views::brand::BrandKey;

/// The same local preview port `vesta_home.rs` stands in for.
const PREVIEW_PORT: u16 = 20_641;

/// Fetch `path` as a public visitor on a launched brand host.
async fn page(host: &str, path: &str) -> String {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    fetch(state, path, host)
        .await
        .expect("launched host answers")
}

/// Fetch `path` through the brand's assigned local preview port, returning
/// the status so the cross-brand 404s can be asserted directly. Local preview
/// admits registered brands independently of the public launch gate.
async fn preview_status(key: BrandKey, path: &str) -> StatusCode {
    let state = AppState {
        canonical_host: CanonicalHost::new(None)
            .with_local_ports([(PREVIEW_PORT, key)].into_iter().collect()),
        ..portal::test_support::app_state(store::test_support::mem_surreal().await).await
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", format!("localhost:{PREVIEW_PORT}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("preview router answers");
    response.status()
}

/// Fetch `path` on `host`, expecting a 200; the body is the page.
async fn fetch(state: AppState, path: &str, host: &str) -> Result<String, StatusCode> {
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body")
            .to_vec(),
    )
    .expect("utf8");
    if status == StatusCode::OK {
        Ok(body)
    } else {
        Err(status)
    }
}

#[tokio::test]
async fn vesta_preview_serves_the_world_page() {
    let html = page_on_preview(BrandKey::Vesta, "/world")
        .await
        .expect("Vesta publishes /world");
    for expected in [
        // The hero and the privacy note.
        "World",
        "Birth details are calculated within your request",
        // The Aster guide, framed from the canonical board.
        "aster-artwork",
        "Aster, the Astrocartographer",
        // The calculated specimen section with its provenance.
        "astro-map-design",
        "Calculated specimen chart",
        "astro-map-provenance",
        // The birth form: labeled rows, country options, staged people.
        "Your chart",
        "Primary person",
        "Select a country…",
        "Add another person",
        "Draw the lines",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
}

/// The same page through a preview port, as a body-or-status.
async fn page_on_preview(key: BrandKey, path: &str) -> Result<String, StatusCode> {
    let state = AppState {
        canonical_host: CanonicalHost::new(None)
            .with_local_ports([(PREVIEW_PORT, key)].into_iter().collect()),
        ..portal::test_support::app_state(store::test_support::mem_surreal().await).await
    };
    fetch(state, path, &format!("localhost:{PREVIEW_PORT}")).await
}

#[tokio::test]
async fn world_renders_on_a_launched_vesta_host() {
    let html = page("www.vestaestateplanning.com", "/world").await;
    assert!(html.contains("aster-artwork"), "{html}");
    assert!(html.contains("Your chart"), "{html}");
}

#[tokio::test]
async fn world_is_vesta_only() {
    for key in BrandKey::ALL {
        if *key == BrandKey::Vesta {
            continue;
        }
        let status = preview_status(*key, "/world").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{key:?} must 404 /world");
    }
}
