//! The monthly and annual privacy offer through the same host router used by the binary.
use axum::{body::Body, http::Request};
use tower::ServiceExt;

#[tokio::test]
async fn privacy_home_publishes_both_plans_with_referrals_and_shared_footer() {
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
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header("host", "staging.deleteyourdata.com")
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
    // The test state configures no deployment host, so sibling links resolve
    // to production; `home_for_host` unit tests cover the staging siblings.
    for required in [
        "Your Life. Less Exposed.",
        "$29/mo",
        "$290/year",
        "Annual — prepaid",
        "non-legal service",
        "Everyone deserves access to justice.",
        "forming an LLC",
        "Prepay $290/year.",
        "Gift cards available",
        "Privacy &#38; Credit",
        "Solana",
        "Record your removal requests on Solana to help document what you requested.",
        "Cyber Injury Law can review whether",
        // The optional death check, for a subscriber with a Vesta estate plan.
        "An optional death check.",
        "Vesta Estate Planning",
        "Visit Vesta Estate Planning",
        "Neon Law Nautilus",
        "sibling-practice__sources",
        "Nev. Rev. Stat. § 688A.140",
        r#"href="https://www.vestaestateplanning.com""#,
        // Cyber crimes, through Cyber Injury Law.
        "Also available for cyber crimes.",
        "Visit CyberInjuryLaw.com",
        r#"href="https://www.cyberinjurylaw.com""#,
        "privacy-cloak",
        "site-footer__legal",
        "https://www.lawyershook.com",
    ] {
        assert!(html.contains(required), "missing {required}");
    }
    for retired in [
        "$30",
        "$300",
        "$20",
        "$200",
        "additional member",
        "starting at",
    ] {
        assert!(!html.contains(retired), "retired offer: {retired}");
    }
    assert_eq!(html.matches("<h1").count(), 1);
    assert!(!html.contains("A little less out there."));
    assert!(!html.contains("Privacy in motion"));
    assert!(!html.contains("Pause motion"));
    assert!(!html.contains("Our litigator friends will happily help you out."));
    assert!(!html.contains("$50"), "the retired annual price survives");
    assert!(!html.contains("Coming soon"));
    assert!(!html.contains("Removal depends on the company"));
    assert!(!html.contains("href=\"/services\""));
    assert_eq!(
        html.matches("href=\"https://calendar.notion.so/meet/nick-shook/or15n4yy7\"")
            .count(),
        4
    );
    assert!(!html.contains("calendar.app.google"));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/services")
                .header("host", "staging.deleteyourdata.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
}
