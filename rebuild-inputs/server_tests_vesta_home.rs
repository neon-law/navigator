//! The estate offer and the firm's two practice doors through the shipped router.
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use portal::{AppState, CanonicalHost};
use tower::ServiceExt;
use views::brand::BrandKey;

/// An arbitrary local port, standing in for whatever
/// `NAVIGATOR_LOCAL_VESTA_PORT` a developer exports. The value is
/// meaningless; that it is *a port the deployment was told to bind* is the
/// whole point.
const PREVIEW_PORT: u16 = 20_640;

/// Fetch `path` as a public visitor on a launched brand host.
async fn page(host: &str, path: &str) -> String {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    fetch(state, path, host).await
}

/// Fetch `path` through the brand's assigned local preview port.
/// Local preview admits registered brands independently of the public launch gate.
async fn preview(key: BrandKey, path: &str) -> String {
    let state = AppState {
        canonical_host: CanonicalHost::new(None)
            .with_local_ports([(PREVIEW_PORT, key)].into_iter().collect()),
        ..portal::test_support::app_state(store::test_support::mem_surreal().await).await
    };
    fetch(state, path, &format!("localhost:{PREVIEW_PORT}")).await
}

async fn fetch(state: AppState, path: &str, host: &str) -> String {
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
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{host} {path}");
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn vesta_shows_starting_price_and_shared_booking_calendar() {
    let html = preview(BrandKey::Vesta, "/").await;
    for expected in [
        "Your estate plan",
        "A planning meeting and attorney preparation and review",
        "A get-to-know-you session with an optional astrology reading",
        "For one signed version",
        "Available by request",
        "individual trustee appointment",
        "trustee appointment",
        "https://calendar.notion.so/meet/nick-shook/or15n4yy7",
        "/public/brand/vesta.svg",
        "Guieb &#38; Shook PC",
        "site-footer",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
    assert!(!html.contains("$3,000"));
    assert!(!html.contains("calendar.app.google"));
    assert!(!html.contains("Coming soon"));
    assert!(!html.contains("not yet available"));
    assert!(html.contains("do not replace the signatures"));
    assert!(!html.contains("Read the video transcript"));
}

#[tokio::test]
async fn lawyer_shook_lists_the_launched_family_and_keeps_the_firm_notice() {
    let html = preview(BrandKey::LawyerShook, "/").await;
    for expected in [
        "holding-page",
        "Guieb &#38; Shook PC is an American law firm",
        "home-practice",
        "Neon Law",
        "DeleteYourData.com",
        "DeleteYourDebt.com",
        "Vesta Estate Planning",
        "Misericordia Injury Law",
        "Abhaya Immigration",
        "Aegis Criminal Defense",
        "Groundwork Real Estate Law",
        "Summons Defense",
        "emerging tech",
        "site-footer",
        "https://www.deleteyourdata.com",
        "https://www.vestaestateplanning.com",
        "https://www.summonsdefense.nyc",
        "/public/brand/lawyer-shook/logo.svg",
        "/public/brand/lawyer-shook/logo.png",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
    let head = html.split_once("</head>").expect("document head").0;
    assert!(head.contains(r#"rel="icon""#), "missing favicon: {head}");
    assert!(
        head.contains(r#"href="/public/brand/lawyer-shook/logo.svg""#),
        "the Lawyer Shook tab must use its supplied mark: {head}"
    );
}

#[tokio::test]
async fn vesta_administration_is_a_separate_contact_engagement() {
    let html = preview(BrandKey::Vesta, "/").await;
    let administration = html
        .split_once("id=\"trust-administration\"")
        .expect("administration section")
        .1
        .split_once("</section>")
        .expect("administration section end")
        .0;
    for expected in [
        "Custom engagement",
        "Contact us about trust administration",
        "before agreeing on work in writing",
        "href=\"https://calendar.notion.so/meet/nick-shook/or15n4yy7\"",
        "href=\"/services#trust-administration\"",
    ] {
        assert!(administration.contains(expected), "missing {expected}");
    }
    assert!(html.contains("href=\"#trust-administration\""));
    assert!(html.find("id=\"nevada-trusts\"") < html.find("id=\"trust-administration\""));

    let services = preview(BrandKey::Vesta, "/services").await;
    for expected in [
        "id=\"trust-administration\"",
        "Custom scope and fees",
        "identify who we represent",
        "conflict review",
        "written engagement",
        "Contact us about trust administration",
    ] {
        assert!(services.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn vesta_leads_with_tailored_planning_and_explains_nevada_trusts() {
    let home = preview(BrandKey::Vesta, "/").await;
    for expected in [
        "Your life is part of a larger story.",
        "Nevada asset protection trust",
        "id=\"nevada-trusts\"",
        "Could a Nevada trust fit your plan?",
        "qualified Nevada trustee",
        "No trust guarantees protection from every claim",
        "probate, trust administration, and later updates require a separate quote",
        "href=\"/services#nevada-trusts\"",
    ] {
        assert!(home.contains(expected), "missing {expected}");
    }
    assert!(home.find("id=\"your-plan\"") < home.find("id=\"nevada-trusts\""));
    assert!(home.find("id=\"nevada-trusts\"") < home.find("<video"));

    let services = preview(BrandKey::Vesta, "/services").await;
    for expected in [
        "id=\"nevada-trusts\"",
        "law may matter",
        "where we may act",
        "scope in writing",
        "No trust guarantees protection from every creditor claim",
    ] {
        assert!(services.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn vesta_services_describe_tailored_planning_with_a_starting_price() {
    let html = preview(BrandKey::Vesta, "/services").await;
    assert!(html.contains("We listen to what matters to you"));
    assert!(html.contains("Basic estate plans for one person start from $1,111"));
    let home = preview(BrandKey::Vesta, "/").await;
    assert!(home.contains("From $1,111"));
    assert!(html.contains("Nicholas Shook may accept appointment"));
    assert!(html.contains("qualified Nevada trustee"));
    assert!(html.contains("Life-insurance trusts"));
    assert!(html.contains("Available by request"));
    assert!(!html.contains("$5 per transaction"));
    assert!(!html.contains("$3,000"));
    assert!(!html.contains("Coming soon"));
    assert!(!html.contains("not yet available"));
}

#[tokio::test]
async fn vesta_and_lawyer_shook_omit_the_lawyer_shook_trademark_notice() {
    for html in [
        preview(BrandKey::Vesta, "/").await,
        preview(BrandKey::LawyerShook, "/").await,
    ] {
        let footer = html.split("<footer").nth(1).expect("shared footer");
        assert!(footer.contains("https://www.guiebandshook.com"));
        assert!(footer.contains("Everyone deserves access to justice."));
        assert!(!footer.contains("Our family"));
        assert!(!footer.contains("LAWYER SHOOK"));
        assert!(!footer.contains("common-law mark of Guieb &#38; Shook PC"));
    }
}

#[tokio::test]
async fn vesta_includes_an_explainer_and_its_registered_typeface() {
    let html = preview(BrandKey::Vesta, "/").await;
    assert!(html.contains("<video"));
    assert!(html.contains("/public/img/vesta-home/vesta-explainer.mp4"));
    assert!(!html.contains("Read the video transcript"));
    assert!(!html.contains("does not replace required signing"));
    let brand = BrandKey::Vesta;
    assert_eq!(brand.default_typeface().id, "eb-garamond");
    assert!(brand.display_typeface().is_none());
}

/// Vesta links back to DeleteYourData.com and names the optional death check
/// a client with an estate plan here can opt in to.
#[tokio::test]
async fn vesta_links_to_delete_your_data_and_its_optional_death_check() {
    let html = page("staging.vestaestateplanning.com", "/").await;
    for expected in [
        "sibling-practices",
        "Pair your plan with DeleteYourData.com.",
        "death check",
        "Visit DeleteYourData.com",
        "deleteyourdata.com\"",
    ] {
        assert!(html.contains(expected), "missing {expected:?}");
    }
}

#[tokio::test]
async fn vesta_process_keeps_engagement_readings_and_later_help_distinct() {
    let html = preview(BrandKey::Vesta, "/").await;
    let process = html.split("id=\"your-plan\"").nth(1).unwrap();
    let mut previous = 0;
    for heading in [
        "Get to know each other",
        "Choose to engage",
        "Plan together",
        "Prepare your estate plan",
        "Meet again to sign",
    ] {
        let position = process
            .find(heading)
            .unwrap_or_else(|| panic!("missing {heading}"));
        assert!(position >= previous);
        previous = position;
    }
    for expected in [
        "For a basic estate plan for one person",
        "A will and financial power of attorney",
        "A health-care directive and health-care power of attorney",
        "One round of revisions before signing",
        "Living trusts, advanced asset protection or tax planning",
        "id=\"astrology\"",
        "id=\"probate\"",
        "required witnesses and notarization",
        "Probate is not needed for every estate",
        "separate scope and fees",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
}

#[tokio::test]
async fn vesta_video_updates_share_the_published_blog_posts() {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    store::seed::seed_brand_blog(&state.surreal, store::seed::BrandSeed::Neon)
        .await
        .unwrap();
    let home = fetch(state.clone(), "/", "staging.vestaestateplanning.com").await;
    assert!(home.contains("Video updates"));
    assert_eq!(home.matches("class=\"vesta-update\"").count(), 5);
    let videos = [
        (
            "where-should-you-go-in-life-20261020",
            "vesta/where-should-you-go-in-life-20261020",
        ),
        ("video-update-2026-10-19", "vesta/video-update-20261019"),
        ("video-update-2026-10-18", "vesta-updates/2026-10-18"),
        ("video-update-2026-10-17", "vesta-updates/2026-10-17"),
        ("video-update-2026-10-16", "vesta-updates/2026-10-16"),
    ];
    let mut previous = None;
    for (slug, asset) in videos {
        let position = home.find(&format!("/blog/{slug}")).unwrap();
        assert!(previous.is_none_or(|earlier| earlier < position));
        previous = Some(position);
        assert!(home.contains(&format!("/assets/img/{asset}.mp4")));
    }
    assert!(!home.contains("/blog/astrology-forecast-2026-10-13"));
    assert!(!home.contains("/blog/the-hardest-part-is-getting-started"));
    assert!(!home.contains("/assets/img/vesta-updates/2026-10-12.mp4"));
    assert!(!home.contains("/assets/img/vesta-updates/2026-10-13.mp4"));
    assert!(home.contains("https://www.facebook.com/reel/1816245906065223/"));
    assert!(
        home.contains("https://www.tiktok.com/@vesta.estate.planning/video/7697979551372086558")
    );
    assert!(!home.contains("Thanks, Apple"));
    let blog = fetch(state.clone(), "/blog", "staging.vestaestateplanning.com").await;
    assert!(blog.contains("October 13, 2026 astrology forecast"));
    assert!(blog.contains("The hardest part is getting started"));
    let post = fetch(
        state,
        "/blog/astrology-forecast-2026-10-13",
        "staging.vestaestateplanning.com",
    )
    .await;
    assert!(post.contains("Astrology readings are for personal reflection, not legal advice."));
    assert!(post.contains("/assets/img/vesta-updates/2026-10-13.mp4"));
    assert!(post.contains("https://www.facebook.com/reel/1118972794002056/"));
}

#[tokio::test]
async fn vesta_cosmos_keeps_rotating_planets_and_reflection_available_without_scripts() {
    let html = preview(BrandKey::Vesta, "/").await;
    for expected in [
        "Your life is part of a larger story.",
        "reaches beyond your lifetime",
        "vesta-orbit--outer",
        "vesta-orbit--middle",
        "vesta-orbit--inner",
        "An imagined sky.",
        "North Node",
        "Mercury",
        "Neptune",
        "Chiron",
        "Venus",
        "Mars",
        "Jupiter",
        "Saturn",
        "Readings are optional personal reflection",
    ] {
        assert!(html.contains(expected), "missing {expected}");
    }
    assert!(!html.contains("vesta-pause"));
    assert!(!html.contains("Still the cosmos"));
    assert_eq!(html.matches("class=\"vesta-theme\"").count(), 8);
    assert_eq!(html.matches("name=\"vesta-reflections\"").count(), 8);
    assert!(html.find("vesta-cosmos").unwrap() < html.find("vesta-offer").unwrap());
}

#[tokio::test]
async fn aster_illustrates_vesta_without_replacing_the_brand_or_optional_reflection() {
    let html = preview(BrandKey::Vesta, "/").await;
    assert!(html.contains("Aster the Astrocartographer"));
    // SVG viewports frame the exact approved raster, retaining its original eyes and texture.
    let source = views::assets::asset_url(views::assets::ASTER_BOARD_KEY);
    assert_eq!(html.matches(&format!("href=\"{source}\"")).count(), 5);
    for pose in ["standing", "explore", "illustrate", "guide"] {
        assert!(
            html.contains(&format!("data-aster-pose=\"{pose}\"")),
            "{pose}"
        );
    }
    assert!(!html.contains("/public/brand/aster/aster.svg"));
    assert!(html.contains("/public/brand/vesta.svg"));
    assert!(html.contains("Readings are optional personal reflection"));
    assert!(!preview(BrandKey::LawyerShook, "/")
        .await
        .contains("/public/brand/aster/"));
    let services = preview(BrandKey::Vesta, "/services").await;
    assert!(services.contains("public-brand-companion"));
    assert!(services.contains(&source));
}
