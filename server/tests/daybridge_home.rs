//! The launched Daybridge divorce practice, reached through its real public
//! hosts (`www.daybridgedivorce.com`, `staging.daybridgedivorce.com`) — the
//! same door a browser or crawler uses now that `Daybridge` is admitted by
//! `BrandKey::LIVE`.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

const HOSTS: [&str; 2] = ["www.daybridgedivorce.com", "staging.daybridgedivorce.com"];

async fn get(host: &str, path: &str) -> axum::http::Response<Body> {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    app.oneshot(
        Request::builder()
            .uri(path)
            .header("host", host)
            .body(Body::empty())
            .expect("request"),
    )
    .await
    .expect("response")
}

async fn body_of(response: axum::http::Response<Body>) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("UTF-8 response")
}

async fn page(host: &str, path: &str) -> String {
    let response = get(host, path).await;
    assert_eq!(response.status(), StatusCode::OK, "{host} {path}");
    body_of(response).await
}

/// The document head must wear Daybridge's own identity — its `og:site_name`,
/// its favicon, and its scoped tokens stylesheet — never Neon Law's.
fn assert_own_brand_head(html: &str, host: &str) {
    let head = html.split_once("</head>").expect("document head").0;
    assert!(
        head.contains(r#"og:site_name" content="Daybridge Divorce Law""#)
            || head.contains(r#"content="Daybridge Divorce Law" property="og:site_name""#),
        "{host} must declare its own og:site_name: {head}"
    );
    assert!(
        head.contains(r#"href="/public/brand/daybridge/logo.svg""#),
        "{host} must use its own favicon/logo: {head}"
    );
    assert!(
        head.contains("/public/brand/daybridge/logo.png"),
        "{host} must share its own logo image: {head}"
    );
    assert!(
        head.contains("/public/css/brand-daybridge-tokens.css"),
        "{host} must load its own scoped brand tokens: {head}"
    );
    assert!(
        !head.contains(r#"og:site_name" content="Neon Law""#)
            && !head.contains(r#"content="Neon Law" property="og:site_name""#),
        "{host} must not fall back to Neon Law's og:site_name: {head}"
    );
}

#[tokio::test]
async fn daybridge_home_leads_with_contact_and_keeps_account_access() {
    for host in HOSTS {
        let html = page(host, "/").await;
        assert_own_brand_head(&html, host);
        for expected in [
            "Daybridge Divorce Law",
            "Prefer to write first?",
            "Contact us to learn more",
            r#"href="/auth/signup?return_to=/app/projects""#,
            r#"href="/auth/login?return_to=/app/projects""#,
            "/public/brand/daybridge/crossing.svg",
            "/public/brand/daybridge/logo.svg",
            "/public/css/daybridge.css",
            "Attorney advertisement",
            "Guieb &#38; Shook PC",
        ] {
            assert!(html.contains(expected), "{host} missing {expected:?}");
        }
        assert!(
            html.contains(r#"href="https://www.vestaestateplanning.com""#),
            "{host} must link Vesta for the estate-plan review"
        );
        let header = html.split("</header>").next().expect("site header");
        assert!(
            header.contains(r#"href="/testimonials""#),
            "{host}: {header}"
        );
        let footer = html.split("<footer").nth(1).expect("shared footer");
        assert!(footer.contains("www.guiebshook.com"), "{host}: {footer}");
        assert!(!footer.contains("Lex Tecnica"), "{host}: {footer}");
        let main = html.split("<footer").next().expect("page body");
        assert!(main.contains(r#"href="/contact""#), "{host}");
        assert!(
            !html.contains("testimonial-section"),
            "{host} has no published quotes"
        );
        assert!(
            !main.contains("<ol") && !main.contains("<li"),
            "{host} home must not use list views"
        );
        let lower = html.to_lowercase();
        for forbidden in [
            "guaranteed result",
            "without delay",
            "$500",
            "$5,000",
            "$5 a day",
        ] {
            assert!(!lower.contains(forbidden), "{host} publishes {forbidden:?}");
        }
    }
}

#[tokio::test]
async fn daybridge_services_explain_scope_and_direct_visitors_to_contact() {
    for host in HOSTS {
        let html = page(host, "/services").await;
        assert_own_brand_head(&html, host);
        for expected in [
            "A brighter way through divorce.",
            "Your agreement in writing",
            "The court sets its",
            "Papers and filing steps",
            "Joint representation starts with clarity",
            "separate counsel",
            "What your engagement covers",
            "discovery expenses, including expert fees",
            "Contact us to learn more",
        ] {
            assert!(html.contains(expected), "{host} missing {expected:?}");
        }
        assert!(html.contains(r#"href="/contact""#), "{host}");
        for price in ["$500", "$5,000", "$5 a day", "$1,000"] {
            assert!(!html.contains(price), "{host} still publishes {price}");
        }
    }
}

/// `/contact` is a shared route every brand answers on its own hosts; it must
/// still publish Daybridge's own mailbox and head, never Neon's.
#[tokio::test]
async fn daybridge_contact_publishes_its_own_mailbox_and_head() {
    for host in HOSTS {
        let html = page(host, "/contact").await;
        assert_own_brand_head(&html, host);
        assert!(
            html.contains("<title>Daybridge Divorce Law | Contact</title>"),
            "{host} must title its own contact page: {html}"
        );
        assert!(
            html.contains("contact@daybridgedivorce.com"),
            "{host} must publish its own mailbox: {html}"
        );
        assert!(html.contains(r#"href="https://calendar.notion.so/meet/nick-shook/or15n4yy7""#));
        assert!(html.contains(r#"href="tel:+15108002090""#));
        assert!(
            !html.contains("contact@neonlaw.com"),
            "{host} must not publish Neon Law's mailbox: {html}"
        );
    }
}

/// `/llms.txt` is a crawler's other door into the site. It must index
/// Daybridge's own two pages under its own name and description, never
/// Neon's.
#[tokio::test]
async fn daybridge_llms_txt_indexes_its_own_pages() {
    for host in HOSTS {
        let response = get(host, "/llms.txt").await;
        assert_eq!(response.status(), StatusCode::OK, "{host} /llms.txt");
        let body = body_of(response).await;
        assert!(
            body.starts_with("# Daybridge Divorce Law\n"),
            "{host} llms.txt must name Daybridge as its own site: {body}"
        );
        assert!(
            body.contains("conflict review") && !body.contains('$'),
            "{host} llms.txt must describe the practice without quoting fees: {body}"
        );
        assert!(
            body.contains(&format!("https://{host}/)")),
            "{host} llms.txt must index its own home page: {body}"
        );
        assert!(
            body.contains(&format!("https://{host}/services)")),
            "{host} llms.txt must index its own services page: {body}"
        );
        assert!(
            !body.contains("Neon Law is a consumer law firm"),
            "{host} llms.txt must not fall back to Neon Law's summary: {body}"
        );
    }
}
