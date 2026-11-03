//! The `nautilus_preview_router` contract — `navigator nautilus preview`'s
//! served surface.
//!
//! Two properties, one per mode the command is honest about:
//!
//! 1. **Degraded (no staged bundle):** the Nautilus home still renders
//!    server-side — the shell is static content, so a missing bundle is the
//!    designed mode the command's banner reports, never a 500 or a blank
//!    page — and with no bundle mounted there is nothing at `/wasm` to
//!    half-load.
//! 2. **Hydrated (a bundle staged under `NAUTILUS_DIOXUS_PUBLIC_PATH`):**
//!    the same shell renders, head-tagged so the wasm may run, and the
//!    staged bundle's wasm and glue are served at the paths the bundle's own
//!    `index.html` references.
//!
//! One test drives both modes because the bundle directory is a
//! process-global environment variable; mutating it between assertions is
//! safe under nextest's process-per-test isolation and race-free under any
//! runner when only one test mutates it. The router propagates the override
//! to dioxus-server's `DIOXUS_PUBLIC_PATH` template knob, so this test
//! cleans that variable up too.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A minimal, CDN-free bundle `index.html`: enough for `dioxus-server` to
/// parse a template with the `main` mount point and render the component
/// into it — the same fixture the `/dioxus-demo` mount test uses. The
/// `data-fixture` marker and the glue `<script>` reference are what the
/// template-follows-the-override assertion keys off.
const INDEX_HTML: &str = "<!DOCTYPE html>\n\
<html lang=\"en\"><head><meta charset=\"UTF-8\" />\n\
<title>Nautilus</title>\n\
<script type=\"module\" src=\"/assets/nautilus-preview.js\"></script></head>\n\
<body><div id=\"main\" data-fixture=\"nautilus-bundle\"></div></body></html>\n";

/// The wasm-bindgen glue a real `dx` build of `nautilus-preview` ships,
/// named for the crate the way `dx` names the webapp bundle's for `webapp`.
const GLUE_JS: &str = "// nautilus-preview browser bundle fixture";

async fn body_string(response: axum::response::Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .expect("collect body")
            .to_bytes()
            .to_vec(),
    )
    .expect("utf-8 body")
}

async fn get(router: axum::Router, uri: &str) -> axum::response::Response {
    router
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router response")
}

#[tokio::test]
async fn the_nautilus_preview_serves_the_mobile_shell_and_degrades_without_a_bundle() {
    // ── Degraded mode: no staged bundle ──
    std::env::remove_var("NAUTILUS_DIOXUS_PUBLIC_PATH");
    let foreign = tempfile::tempdir().expect("foreign bundle");
    std::fs::write(
        foreign.path().join("index.html"),
        INDEX_HTML.replace("nautilus-preview.js", "foreign-app.js"),
    )
    .expect("foreign template");
    std::env::set_var("DIOXUS_PUBLIC_PATH", foreign.path());
    let router = portal::dioxus_app::nautilus_preview_router();

    let response = get(router, portal::dioxus_app::NAUTILUS_PREVIEW_PATH).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_string(response).await;
    assert!(
        !html.contains("foreign-app.js") && !html.contains("data-fixture"),
        "the SSR-only shell must ignore another app's template: {html}"
    );
    assert!(
        html.contains("<h1>Nautilus</h1>"),
        "the subscription home must render server-side before any hydration: {html}"
    );
    assert!(
        html.contains("$29/month") && html.contains("$290/year"),
        "the pricing the shell sells is in the SSR markup: {html}"
    );
    assert!(
        html.contains(r#"data-mobile-nav="dyd""#),
        "the navigation hook the native shell taps must be in the SSR markup: {html}"
    );
    assert!(
        html.contains("data-nautilus-preview-banner"),
        "the degraded page itself must carry the local-render notice: {html}"
    );

    // Degraded means SSR-only: with no bundle mounted there is no wasm to
    // half-load, so a browser that somehow asks for one is told plainly.
    let router = portal::dioxus_app::nautilus_preview_router();
    let response = get(router, "/wasm/nautilus-preview.js").await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "no staged bundle, no wasm route"
    );

    // ── Hydrated mode: a bundle staged under the override ──
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("index.html"), INDEX_HTML).expect("write index.html");
    std::fs::create_dir(dir.path().join("wasm")).expect("dx wasm directory");
    std::fs::write(dir.path().join("wasm/nautilus-preview.js"), GLUE_JS).expect("write glue");
    std::fs::create_dir(dir.path().join("assets")).expect("dx assets directory");
    std::fs::write(
        dir.path().join("assets/nautilus-preview.js"),
        "// optimized glue fixture",
    )
    .expect("write assets glue");
    std::env::set_var("NAUTILUS_DIOXUS_PUBLIC_PATH", dir.path());

    let router = portal::dioxus_app::nautilus_preview_router();
    let response = get(router, portal::dioxus_app::NAUTILUS_PREVIEW_PATH).await;
    assert_eq!(response.status(), StatusCode::OK);
    // The route scopes a nonce'd CSP so Dioxus's inline hydration scripts run
    // under `script-src 'self' 'nonce-…' 'wasm-unsafe-eval'` — the same
    // contract the `/dioxus-demo` mount holds.
    let csp = response
        .headers()
        .get("content-security-policy")
        .and_then(|value| value.to_str().ok())
        .expect("rendered page must carry a CSP")
        .to_string();
    assert!(
        csp.contains("script-src 'self' 'nonce-") && csp.contains("'wasm-unsafe-eval'"),
        "CSP must nonce the hydration scripts and allow wasm: {csp}"
    );
    let html = body_string(response).await;
    assert!(
        html.contains(r#"data-mobile-nav="dyd""#) && html.contains("Nautilus"),
        "the shell renders identically with a bundle staged: {html}"
    );
    // The template — not just the assets — follows the override: the served
    // document is the staged bundle's own index.html, carrying its root
    // markup and loader reference. This is the contract that breaks when the
    // router points only the assets mount at the override.
    assert!(
        html.contains(r#"data-fixture="nautilus-bundle""#)
            && html.contains(r#"src="/assets/nautilus-preview.js""#),
        "the staged bundle's own index.html is the served template: {html}"
    );
    assert!(
        html.contains("data-nautilus-preview-banner"),
        "the hydrated page itself must carry the local-render notice: {html}"
    );

    // The staged bundle's wasm and glue are served at the paths its own
    // index.html references — what turns the SSR shell into a hydrated app.
    let router = portal::dioxus_app::nautilus_preview_router();
    let response = get(router, "/wasm/nautilus-preview.js").await;
    assert_eq!(response.status(), StatusCode::OK, "wasm glue must serve");
    assert_eq!(body_string(response).await, GLUE_JS);

    let router = portal::dioxus_app::nautilus_preview_router();
    let response = get(router, "/assets/nautilus-preview.js").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "optimized assets must serve"
    );

    std::env::remove_var("NAUTILUS_DIOXUS_PUBLIC_PATH");
    std::env::remove_var("DIOXUS_PUBLIC_PATH");
}
