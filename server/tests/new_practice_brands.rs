//! Prepared criminal-defense and real-estate sites render through local brand ports.

use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
};
use tower::ServiceExt;
use views::brand::BrandKey;

#[tokio::test]
async fn prepared_practice_pages_render_their_own_identity_and_contact_form() {
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    state.canonical_host = portal::CanonicalHost::new(None).with_local_ports(
        [(21301, BrandKey::Aegis), (21302, BrandKey::Groundwork)]
            .into_iter()
            .collect(),
    );
    let db = state.surreal.clone();
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    for (key, port, other_name, headline) in [
        (
            BrandKey::Aegis,
            21301,
            "Groundwork Real Estate Law",
            "Facing criminal charges? Start with a conversation.",
        ),
        (
            BrandKey::Groundwork,
            21302,
            "Aegis Criminal Defense",
            "Before you sign, know where you stand.",
        ),
    ] {
        let branding = key.resolve_branding(&views::brand::DEFAULT_BRANDING);
        for path in [
            "/",
            "/services",
            "/contact",
            "/explainers/first-steps",
            "/explainers/process",
            "/explainers/documents",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", format!("localhost:{port}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{key:?} {path}");
            let csrf_cookie = response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .find(|value| value.starts_with("navigator_lead_csrf="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned);
            let html = String::from_utf8(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body")
                    .to_vec(),
            )
            .expect("UTF-8");
            assert!(
                html.contains(branding.firm.site_name),
                "own identity: {key:?} {path}"
            );
            assert!(html.contains("Guieb &#38; Shook PC"), "engagement identity");
            assert!(
                html.contains(views::brand::SHOOK_CONSULTATION_URL),
                "shared Notion booking link"
            );
            assert!(
                !html.contains(other_name),
                "unlaunched sibling is not advertised"
            );
            assert!(
                html.contains(&format!("brand-{}-tokens.css", key.as_str())),
                "own palette"
            );
            if path == "/" {
                assert!(html.contains(headline), "practice headline");
                assert!(html.contains("/leads"), "contact form");
                assert_lead_attribution(
                    &app,
                    &db,
                    key,
                    port,
                    csrf_cookie.expect("lead CSRF cookie"),
                    &html,
                )
                .await;
                assert!(html.contains(branding.firm.logo_href), "own mark");
            }
        }
    }
}

async fn assert_lead_attribution(
    app: &axum::Router,
    db: &store::surreal::SurrealDb,
    key: BrandKey,
    port: u16,
    csrf_cookie: String,
    html: &str,
) {
    let document = scraper::Html::parse_document(html);
    let inputs = scraper::Selector::parse(r#"form[action="/leads"] input[type="hidden"]"#)
        .expect("hidden input selector");
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    for input in document.select(&inputs) {
        form.append_pair(
            input.value().attr("name").expect("input name"),
            input.value().attr("value").unwrap_or(""),
        );
    }
    form.append_pair("email", &format!("{}@example.com", key.as_str()));
    form.append_pair("website", "");
    let submit = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/leads")
                .header(header::HOST, format!("localhost:{port}"))
                .header(header::COOKIE, csrf_cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(form.finish()))
                .expect("lead request"),
        )
        .await
        .expect("lead response");
    assert_eq!(submit.status(), StatusCode::SEE_OTHER);
    assert_eq!(submit.headers()[header::LOCATION], "/contact/sent");
    assert!(
        store::leads::list(db)
            .await
            .expect("saved leads")
            .iter()
            .any(|lead| lead.brand_key == key.as_str() && lead.source_path == "/"),
        "lead attributed by the request host"
    );
}

#[tokio::test]
async fn launched_domains_serve_their_own_brand_and_apexes_redirect_to_their_own_www() {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    for key in [BrandKey::Aegis, BrandKey::Groundwork] {
        assert!(key.is_live(), "{key:?} has launched");
        let site_name = key
            .resolve_branding(&views::brand::DEFAULT_BRANDING)
            .firm
            .site_name;
        for host in key
            .hosts()
            .iter()
            .copied()
            .chain(std::iter::once(key.apex()))
        {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("host", host)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            if host == key.apex() {
                assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY, "{host}");
                assert_eq!(
                    response.headers()[header::LOCATION],
                    format!("https://{}/", key.canonical_host()).as_str(),
                    "{host} redirects to its own brand, not another"
                );
            } else {
                assert_eq!(response.status(), StatusCode::OK, "{host}");
                let html = String::from_utf8(
                    to_bytes(response.into_body(), usize::MAX)
                        .await
                        .expect("body")
                        .to_vec(),
                )
                .expect("UTF-8");
                assert!(html.contains(site_name), "{host} wears {site_name}");
            }
        }
    }
}

#[tokio::test]
async fn explainers_are_discoverable_and_do_not_leak_to_other_brands() {
    let mut state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    state.canonical_host = portal::CanonicalHost::new(None).with_local_ports(
        [
            (21301, BrandKey::Aegis),
            (21302, BrandKey::Groundwork),
            (21303, BrandKey::Neon),
        ]
        .into_iter()
        .collect(),
    );
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    for (key, port, expected) in [
        (BrandKey::Aegis, 21301, "How a criminal case moves"),
        (
            BrandKey::Groundwork,
            21302,
            "Reading a lease before signing",
        ),
    ] {
        for path in ["/explainers/process", "/sitemap.xml", "/llms.txt"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::HOST, format!("localhost:{port}"))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK);
            let body = String::from_utf8(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body")
                    .to_vec(),
            )
            .expect("UTF-8");
            if path == "/explainers/process" {
                assert!(body.contains(expected), "own explainer copy");
                let document = scraper::Html::parse_document(&body);
                let links = scraper::Selector::parse("header a").expect("header links");
                assert!(
                    document.select(&links).any(|link| {
                        link.text()
                            .collect::<String>()
                            .trim()
                            .starts_with("Contact")
                            && link.value().attr("href")
                                == Some(views::brand::SHOOK_CONSULTATION_URL)
                    }),
                    "navigation Contact opens Notion"
                );
            } else {
                for (route, _) in key.explainer_pages() {
                    assert!(body.contains(route), "crawler discovers {route}");
                }
            }
        }
    }
    for path in [
        "/explainers/first-steps",
        "/explainers/process",
        "/explainers/documents",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header(header::HOST, "localhost:21303")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "other brand {path}"
        );
    }
}
