//! Public pages project database-authored Posts and confirmed provider receipts.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use chrono::Utc;
use portal::{AppState, CanonicalHost};
use store::posts::{SourceCaptions, SourcePost};
use tower::ServiceExt;
use views::brand::BrandKey;

const PREVIEW_PORT: u16 = 20_641;
const FACEBOOK: &str = "https://www.facebook.com/synthetic/posts/generated-facebook";
const TIKTOK: &str = "https://www.tiktok.com/@synthetic/video/generated-tiktok";
const FOREIGN_LINK: &str = "https://www.tiktok.com/@synthetic/video/foreign-receipt";

async fn fetch(app: &axum::Router, path: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", format!("localhost:{PREVIEW_PORT}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

async fn fixture() -> (store::surreal::SurrealDb, uuid::Uuid, axum::Router) {
    let db = store::test_support::mem_surreal().await;
    let now = Utc::now();
    let source = SourcePost {
        brand_key: "vesta",
        slug: "generated-receipt-post",
        title: "Synthetic database update",
        description: "A synthetic caption.",
        body_html: "<p>A synthetic database article.</p>",
        body_markdown: None,
        status: "published",
        publish_at: now - chrono::Duration::hours(1),
        video_asset: Some("img/vesta/synthetic-generated.mp4"),
        vertical_video: None,
        tiktok_ai_generated: Some(false),
        captions: SourceCaptions::default(),
        accounts: &[],
        social_links: &[],
    };
    let id = store::posts::import_source(&db, &source, now)
        .await
        .unwrap()
        .post_id;
    let foreign_source = SourcePost {
        brand_key: "neon",
        slug: "foreign-receipt-post",
        video_asset: Some("img/neon/synthetic-generated.mp4"),
        ..source
    };
    let foreign = store::posts::import_source(&db, &foreign_source, now)
        .await
        .unwrap()
        .post_id;
    record_receipts(&db, id, foreign).await;
    let state = AppState {
        canonical_host: CanonicalHost::new(None)
            .with_local_ports([(PREVIEW_PORT, BrandKey::Vesta)].into_iter().collect()),
        ..portal::test_support::app_state(db.clone()).await
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    (db, id, app)
}

async fn record_receipts(db: &store::surreal::SurrealDb, id: uuid::Uuid, foreign: uuid::Uuid) {
    for (post, provider, remote, link) in [
        (id, "facebook", "generated-facebook", Some(FACEBOOK)),
        (id, "tiktok", "generated-tiktok", None),
        (foreign, "tiktok", "foreign-receipt", Some(FOREIGN_LINK)),
    ] {
        store::social_publications::admit(db, post, provider)
            .await
            .unwrap();
        store::social_publications::mark_confirmed(db, post, provider, remote, link)
            .await
            .unwrap();
    }
    store::social_publications::admit(db, id, "instagram")
        .await
        .unwrap();
    store::social_publications::mark_unknown(db, id, "instagram", "synthetic unknown")
        .await
        .unwrap();
    store::social_publications::admit(db, id, "threads")
        .await
        .unwrap();
    store::social_publications::mark_failed(db, id, "threads", "synthetic failure")
        .await
        .unwrap();
}

#[tokio::test]
async fn database_posts_show_confirmed_receipts_on_article_and_home_without_seed_changes() {
    let (db, id, app) = fixture().await;
    for path in ["/blog/generated-receipt-post", "/"] {
        let html = fetch(&app, path).await;
        assert!(html.contains(FACEBOOK), "{path}: {html}");
        assert!(!html.contains(TIKTOK), "{path}: pending permalink exposed");
        assert!(
            !html.contains(FOREIGN_LINK),
            "{path}: foreign receipt exposed"
        );
        assert_eq!(
            html.matches(&format!("href=\"{FACEBOOK}\"")).count(),
            1,
            "{path}: duplicate provider"
        );
    }
    store::social_publications::record_permalink(&db, id, "tiktok", TIKTOK)
        .await
        .unwrap();
    for path in ["/blog/generated-receipt-post", "/"] {
        let html = fetch(&app, path).await;
        for link in [FACEBOOK, TIKTOK] {
            assert_eq!(
                html.matches(&format!("href=\"{link}\"")).count(),
                1,
                "{path}: {html}"
            );
        }
        assert!(
            !html.contains(FOREIGN_LINK),
            "{path}: foreign receipt exposed"
        );
    }
    assert!(store::posts::find_by_id(&db, id)
        .await
        .unwrap()
        .unwrap()
        .social_links
        .is_empty());
}
