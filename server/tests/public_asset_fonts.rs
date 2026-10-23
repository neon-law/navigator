//! A shared public asset origin must serve fonts to every house brand.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn public_fonts_allow_anonymous_cross_origin_get_and_head() {
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let root = tempfile::tempdir().expect("isolated public asset storage");
    state.assets_storage = std::sync::Arc::new(
        cloud::FsStorage::new(root.path())
            .await
            .expect("public storage"),
    );
    state
        .assets_storage
        .put("fonts/example/Regular.woff2", b"wOF2fixture", "font/woff2")
        .await
        .expect("stage public font");
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    for (host, origin) in [
        ("www.neonlaw.com", "https://www.deleteyourdebt.com"),
        ("staging.neonlaw.com", "https://staging.deleteyourdebt.com"),
    ] {
        for method in ["GET", "HEAD"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/assets/fonts/example/Regular.woff2")
                        .header("host", host)
                        .header("origin", origin)
                        .body(Body::empty())
                        .expect("font request"),
                )
                .await
                .expect("font response");
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["access-control-allow-origin"], "*");
            assert_eq!(response.headers()["content-type"], "font/woff2");
            assert!(!response
                .headers()
                .contains_key("access-control-allow-credentials"));
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("font body");
            assert_eq!(
                body.as_ref(),
                if method == "HEAD" {
                    &b""[..]
                } else {
                    &b"wOF2fixture"[..]
                }
            );
        }
    }
}
