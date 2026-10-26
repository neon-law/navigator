//! Every brand in the public directory renders its registered, bundled icon.
//! Neon Law itself is left out: the directory already sits on Neon Law's site.
//! Lawyer Shook is left out: the directory lists practices, not the holding page.
//!
//! The icon is the tracked `/public/brand/...` mark the app serves itself, not
//! an assets-bucket URL: tracked marks are never uploaded to the bucket, so a
//! bucket URL 404s in production.
use axum::{body::Body, http::Request};
use tower::ServiceExt;
use views::brand::{BrandKey, DEFAULT_BRANDING};

#[tokio::test]
async fn services_directory_renders_every_brand_icon() {
    // Production's bucket origin. Set before anything reads the lazily cached
    // asset base, so a logo routed through `asset_url` would show up here.
    std::env::set_var("NAVIGATOR_ASSET_BASE_URL", "https://www.neonlaw.com/assets");
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let dioxus = neon::public_dioxus_routers(&state);
    let app = portal::bootstrap(
        state,
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
        neon::public_routes(),
        neon::PUBLIC_PATHS,
        dioxus,
    )
    .expect("public router");
    let response = app
        .oneshot(
            Request::builder()
                .uri("/services")
                .header("host", "www.neonlaw.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let brands: Vec<BrandKey> = BrandKey::ALL
        .iter()
        .copied()
        .filter(|key| {
            !matches!(
                key,
                BrandKey::Neon | BrandKey::LawyerShook | BrandKey::GuiebShook
            )
        })
        .collect();
    assert_eq!(
        html.matches("class=\"fm-card__logo\"").count(),
        brands.len()
    );
    assert!(
        !html.contains("Explore Neon Law"),
        "redundant Neon Law card"
    );
    let directory = html
        .split("id=\"fees\"")
        .next()
        .expect("the fee catalog follows the brand directory");
    assert!(
        !directory.contains("fm-chip"),
        "brand cards carry no practice-area pills: {directory}"
    );
    assert!(
        !directory.contains("lawyer-shook") && !directory.contains("Meet Lawyer Shook"),
        "Lawyer Shook is not a services-directory card: {directory}"
    );
    for key in brands {
        let branding = key.resolve_branding(&DEFAULT_BRANDING);
        let logo = branding.firm.logo_href;
        assert!(
            html.contains(&format!("src=\"{logo}\"")),
            "{} icon missing",
            key.as_str()
        );
        let asset = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../server/public")
            .join(logo.trim_start_matches("/public/"));
        assert!(asset.is_file(), "{} icon asset missing", key.as_str());
    }
}
