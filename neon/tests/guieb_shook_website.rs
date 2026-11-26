//! The public redesign uses the same mounted router as the deployed brand.
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn the_new_site_publish_routes_and_navigator_entry_points_are_brand_scoped() {
    let state = portal::test_support::app_state(store::surreal::SurrealDb::uninitialized()).await;
    let dioxus = neon::public_dioxus_routers(&state);
    let app = portal::bootstrap(
        state,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
        neon::public_routes(),
        neon::PUBLIC_PATHS,
        dioxus,
    )
    .expect("public router");
    let get = |host, path| {
        app.clone().oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .unwrap(),
        )
    };
    let host = "staging.guiebshook.com";
    for (path, marker) in [
        ("/", "Modern tools."),
        ("/our-story", "Built in New York,"),
        ("/practice/business-litigation", "Business Litigation"),
        ("/attorneys/nicholas-shook", "Nicholas Shook"),
        ("/legal/privacy-policy", "Information you send us"),
        ("/contact", "How can we help?"),
        ("/contact/sent", "Thank you for reaching out."),
    ] {
        let response = get(host, path).await.unwrap();
        assert_eq!(response.status(), 200, "{path}");
        if path == "/contact" {
            assert!(response.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("navigator_lead_csrf"));
        }
        let bytes = axum::body::to_bytes(response.into_body(), 300_000)
            .await
            .unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains(marker), "{path} is missing {marker}");
        assert!(html.contains(r#"href="/auth/login""#));
        assert!(html.contains("guieb-shook-website.css"));
        assert!(!html.contains(r#".html""#), "{path} links a .html path");
        if path == "/" {
            assert!(html.contains(r#"class="nav-slide-dot is-active""#));
        }
        if path == "/contact" {
            assert!(html.contains(r#"action="/leads""#));
            assert!(html.contains(r#"name="concern""#));
            assert!(html.contains(r#"name="csrf_token""#));
            assert!(html.contains(r#"for="cf-email""#));
            assert!(html.contains(r#"name="email""#));
            assert!(html.contains(r#"autocomplete="email""#));
        }
    }
    for (old, new) in [
        ("/our-story.html", "/our-story"),
        (
            "/practice/business-litigation.html",
            "/practice/business-litigation",
        ),
    ] {
        let response = get(host, old).await.unwrap();
        assert_eq!(response.status(), 308, "{old}");
        assert_eq!(response.headers()["location"], new, "{old}");
    }
    assert_eq!(
        get("www.neonlaw.com", "/practice/business-litigation")
            .await
            .unwrap()
            .status(),
        404
    );
}
