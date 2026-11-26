//! HTTP fixture admission, isolation, and cleanup without a browser or Source.

#[path = "common/practice_preview.rs"]
mod practice_preview;

use practice_preview::PracticePreview;
use store::postgres::PostgresDb;

fn page_text(body: &str) -> String {
    let document = scraper::Html::parse_document(body);
    let selector = scraper::Selector::parse("body").unwrap();
    let page = document.select(&selector).next().unwrap();
    let mut text = String::new();
    for node in page.descendants() {
        if let Some(value) = node.value().as_text() {
            let script_or_style = node
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .any(|element| matches!(element.value().name(), "script" | "style"));
            if !script_or_style {
                text.push_str(value);
            }
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn hydration_metadata_cannot_substitute_for_rendered_brand_content() {
    assert!(page_text(
        "<title>Brand</title><style>Brand</style><body><script>Brand</script></body>"
    )
    .is_empty());
    assert_eq!(
        page_text("<main>Brand <strong>content</strong></main>"),
        "Brand content"
    );
}

#[tokio::test]
async fn independent_brand_previews_bind_their_own_pool_and_recheck_admission() {
    let names = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let run = || {
        let names = names.clone();
        let barrier = barrier.clone();
        PracticePreview::run(move |preview| {
            Box::pin(async move {
                let pg = PostgresDb::application().await.unwrap();
                let database: String = sqlx::query_scalar("SELECT current_database()")
                    .fetch_one(pg.pool())
                    .await
                    .unwrap();
                names.lock().unwrap().push(database);
                barrier.wait().await;
                let client = reqwest::Client::new();
                for (brand, origin) in &preview.sites {
                    let response = client.get(origin).send().await.unwrap();
                    assert!(response.status().is_success());
                    let body = response.text().await.unwrap();
                    let expected = brand
                        .resolve_branding(&views::brand::DEFAULT_BRANDING)
                        .firm
                        .site_name;
                    assert!(
                        page_text(&body).contains(expected),
                        "preview {brand:?} must render {expected}"
                    );
                }
                let damaged_schema = sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM public._sqlx_migrations)")
                    .execute(pg.pool())
                    .await
                    .unwrap();
                assert_eq!(
                    damaged_schema.rows_affected(),
                    1,
                    "the owned native schema attestation must actually be damaged"
                );
                let response = client.get(&preview.sites[0].1).send().await.unwrap();
                assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
            })
        })
    };
    tokio::join!(run(), run());
    let names = names.lock().unwrap();
    if !names.is_empty() {
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
    }
}

#[tokio::test]
async fn a_browser_assertion_panic_drains_http_and_drops_only_its_fixture() {
    use futures::FutureExt as _;
    let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
    let shared = observed.clone();
    let outcome = std::panic::AssertUnwindSafe(PracticePreview::run(move |preview| {
        Box::pin(async move {
            let pg = PostgresDb::application().await.unwrap();
            let database: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(pg.pool())
                .await
                .unwrap();
            let admin_database = std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                .unwrap_or_else(|_| "smoke".into());
            let options = pg
                .pool()
                .connect_options()
                .as_ref()
                .clone()
                .database(&admin_database);
            *shared.lock().unwrap() = Some((database, options, preview.sites.clone()));
            panic!("synthetic browser assertion");
        })
    }))
    .catch_unwind()
    .await;
    let Some((database, options, sites)) = observed.lock().unwrap().take() else {
        std::panic::resume_unwind(
            outcome.expect_err("configured preview must reach the assertion callback"),
        );
    };
    assert!(outcome.is_err());
    let admin = PostgresDb::connect(options).await.unwrap();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_database WHERE datname=$1)")
            .bind(database)
            .fetch_one(admin.pool())
            .await
            .unwrap();
    assert!(
        !exists,
        "panicking browser must not leak its owned database"
    );
    for (_, origin) in sites {
        assert!(
            reqwest::get(origin).await.is_err(),
            "HTTP listener drained before cleanup"
        );
    }
    let alive: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(admin.pool())
        .await
        .unwrap();
    assert_eq!(
        alive, 1,
        "fixture cleanup preserves its administrative parent"
    );
    admin.pool().close().await;
}
