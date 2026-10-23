//! Summons Defense publishes its offer, intake, and official guides on each brand host.
use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use portal::CanonicalHost;
use tower::ServiceExt;
use views::brand::BrandKey;

#[tokio::test]
async fn summons_publishes_offer_and_contact_on_production_and_staging() {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    for host in BrandKey::Summons.hosts() {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header("host", *host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{host}");
        let html = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(
            html.contains("width=device-width, initial-scale=1"),
            "{host}"
        );
        assert!(html.contains("$100"), "{host}");
        assert!(html.contains("$500"), "{host}");
        assert!(html.contains("Tell us once."), "{host}");
        assert!(html.contains("action=\"/leads\""), "{host}");
        assert!(html.contains("name=\"_csrf\""), "{host}");
        assert!(html.contains("https://www.nyc.gov/site/oath/"), "{host}");
        assert!(!html.contains("Coming Soon"), "{host}");
        assert!(!html.contains("$50 flat"), "{host}");
        assert!(
            html.contains("/public/css/brand-summons-tokens.css"),
            "{host}"
        );
        let head = html.split_once("</head>").expect("document head").0;
        assert!(head.contains(r#"rel="icon""#), "{host}: {head}");
        assert!(
            head.contains(r#"href="/public/brand/summons/logo.svg""#),
            "{host} must use the Summons mark as its favicon: {head}"
        );
        assert!(html.contains("Libre Franklin"), "{host}");
        assert!(!html.contains("GORP"), "{host}");
        assert!(!html.contains("holding-page__paragraph"), "{host}");
        assert!(!html.contains("home-practice"), "{host}");
        assert!(html.contains("Attorney advertisement"), "{host}");
        assert!(html.contains("THE PROCESS"), "{host}");
        assert!(html.contains("KNOW YOUR SUMMONS"), "{host}");
        assert!(!html.contains("01 /"), "{host}");
        assert!(!html.contains("02 /"), "{host}");
        assert!(!html.contains("03 /"), "{host}");
        assert!(!html.contains("class=\"summons-number\""), "{host}");
        for path in ["/services", "/contact", "/contact/sent"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", *host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{host}{path}");
            assert!(
                response.headers().get(header::LOCATION).is_none(),
                "{host}{path} must not redirect anywhere, got Location: {:?}",
                response.headers().get(header::LOCATION)
            );
            let body = String::from_utf8(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .unwrap()
                    .to_vec(),
            )
            .unwrap();
            assert!(
                !body.contains("Coming Soon"),
                "{host}{path} must not contain a launch placeholder"
            );
            assert!(
                !body.contains("Summons Defense | Coming Soon"),
                "{host}{path} must have the published page title"
            );
        }
    }
}

#[tokio::test]
async fn summons_apex_redirects_to_its_own_site() {
    let state = portal::AppState {
        canonical_host: CanonicalHost::new(Some("www.neonlaw.com".into())),
        ..portal::test_support::app_state(store::test_support::mem_surreal().await).await
    };
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("host", BrandKey::Summons.apex())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://www.summonsdefense.nyc/"
    );
}

/// Exercise the actual page-issued token and brand resolution through the write route.
#[tokio::test]
async fn summons_intake_saves_a_brand_scoped_contact_request() {
    let db = store::test_support::mem_surreal().await;
    let state = portal::test_support::app_state(db.clone()).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/")
                .header("host", "www.summonsdefense.nyc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().split(';').next().unwrap())
        .collect::<Vec<_>>()
        .join("; ");
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    let token = html
        .split("name=\"_csrf\"")
        .nth(1)
        .unwrap()
        .split("value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert!(!token.is_empty());
    let response = app.clone().oneshot(Request::builder().method("POST").uri("/leads")
        .header("host", "www.summonsdefense.nyc")
        .header(header::COOKIE, cookies)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(format!("email=summons-intake%40example.com&_csrf={token}&source_path=%2F&consent_version=contact-request")))
        .unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/contact/sent");
    let leads = store::leads::list(&db).await.unwrap();
    assert_eq!(leads.len(), 1);
    assert_eq!(leads[0].brand_key, "summons");
    assert_eq!(leads[0].source_path, "/");
    assert_eq!(leads[0].email_lower, "summons-intake@example.com");
    let confirmation = app
        .oneshot(
            Request::builder()
                .uri("/contact/sent")
                .header("host", "www.summonsdefense.nyc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(confirmation.status(), StatusCode::OK);
}
