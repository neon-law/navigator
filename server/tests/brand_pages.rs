//! End-to-end title and content coverage for the four launched practice sites.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

async fn page(host: &str, path: &str) -> String {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .expect("brand page request"),
        )
        .await
        .expect("brand page response");
    assert_eq!(response.status(), StatusCode::OK, "{host} {path}");
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("brand page body")
            .to_vec(),
    )
    .expect("brand page is UTF-8")
}

async fn assert_brand_pages(
    host: &str,
    title: &str,
    home_copy: &str,
    services_copy: &str,
    tokens_css: &str,
) {
    let home = page(host, "/").await;
    for expected in [
        title,
        home_copy,
        "site-footer",
        tokens_css,
        "Guieb &#38; Shook PC",
    ] {
        assert!(
            home.contains(expected),
            "{host} home is missing {expected:?}"
        );
    }
    assert!(
        !home.contains("Coming Soon"),
        "{host} still renders a holding page"
    );

    let services = page(host, "/services").await;
    for expected in [services_copy, "site-footer", tokens_css] {
        assert!(
            services.contains(expected),
            "{host} services is missing {expected:?}"
        );
    }
    assert!(
        !services.contains("Coming Soon"),
        "{host} services still renders a holding page"
    );
}

#[tokio::test]
async fn vesta_publishes_estate_planning_home_and_services() {
    assert_brand_pages(
        "staging.vestaestateplanning.com",
        "<title>Vesta Estate Planning | Home</title>",
        "Your life is part of a larger story.",
        "Custom scope and fees",
        "/public/css/brand-vesta-tokens.css",
    )
    .await;
}

#[tokio::test]
async fn misericordia_publishes_injury_home_and_services() {
    assert_brand_pages(
        "staging.misericordialaw.com",
        "<title>Misericordia Injury Law | Home</title>",
        "You were hurt. We are here to listen.",
        "How we are paid",
        "/public/css/brand-misericordia-tokens.css",
    )
    .await;
}

#[tokio::test]
async fn abhaya_publishes_immigration_home_and_services() {
    assert_brand_pages(
        "staging.abhayaimmigration.com",
        "<title>Abhaya Immigration | Home</title>",
        "Your next chapter, without fear.",
        "Immigration services",
        "/public/css/brand-abhaya-tokens.css",
    )
    .await;
}

/// Abhaya's flat fees come from its own services catalog through the shared
/// services band, not from prose cards, so the page carries the same search
/// form and government-fee badge as every other catalog-backed brand.
#[tokio::test]
async fn abhaya_services_lists_flat_fees_for_common_filings() {
    let services = page("staging.abhayaimmigration.com", "/services").await;
    for expected in [
        "Flat fees for common filings",
        "O-1 extraordinary ability petition",
        "$7,200",
        "Green card application in the U.S.",
        "$3,150",
        "Government fees cost extra",
        "Work visas",
    ] {
        assert!(
            services.contains(expected),
            "Abhaya services is missing {expected:?}"
        );
    }
}

#[tokio::test]
async fn delete_your_debt_publishes_panda_home_and_services() {
    assert_brand_pages(
        "staging.deleteyourdebt.com",
        "<title>DeleteYourDebt.com | Home</title>",
        "A next step beyond debt.",
        "Debt relief with PandA Law",
        "/public/css/brand-delete-your-debt-tokens.css",
    )
    .await;
}

#[tokio::test]
async fn debt_home_connects_panda_and_brooklyn_on_both_hosts() {
    for host in ["www.deleteyourdebt.com", "staging.deleteyourdebt.com"] {
        let home = page(host, "/").await;
        for expected in [
            r#"class="debt-home""#,
            r#"href="https://www.pandalawfirm.com/contact/""#,
            "Nevada",
            "Washington",
            "New York",
            "California",
            "Brooklyn",
        ] {
            assert!(home.contains(expected), "{host} missing {expected}");
        }

        assert!(!home.contains("We do not settle debts"));
    }
}
