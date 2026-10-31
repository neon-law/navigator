//! `CyberInjuryLaw`'s public Dioxus home, on its own launched hosts.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;
use views::brand::BrandKey;

const HOSTS: [&str; 2] = ["www.cyberinjurylaw.com", "staging.cyberinjurylaw.com"];

async fn page(host: &str, path: &str) -> (StatusCode, String) {
    let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", host)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = response.status();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("UTF-8 response");
    (status, body)
}

#[tokio::test]
async fn cyber_injury_home_serves_its_copy_assets_and_fonts() {
    for host in HOSTS {
        let (status, html) = page(host, "/").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for expected in [
            "CyberInjuryLaw",
            "WE USE AI TO",
            "YOUR POCKET.",
            "AI-ENHANCED PORTRAIT",
            "CYBERCRIME.",
            "Car accident or other collision",
            "online abuse",
            "cyber-injury-law.css",
            "BarlowCondensed-ExtraBold.woff2",
            "DMSans-Regular.woff2",
            "cyber-warrior-ferris.png",
            "canal-street-ad-f9faa356.png",
            "Subway ad",
            "https://www.cyberinjurylaw.com/android",
            "https://www.cyberinjurylaw.com/iPhone",
            "cyber-android-qr.svg",
            "cyber-iphone-qr.svg",
            "cyber-phone-qr.svg",
            "cyber-testimonials-qr.svg",
            "tel:+15108002090",
            "1.510.800.2090",
            "support@cyberinjurylaw.com",
            "href=\"https://www.cyberinjurylaw.com/testimonials\"",
            "cyber-incident",
            "cyber-timing",
            "Guieb &#38; Shook PC",
            "site-footer",
            "Law.com",
            "cyber-consultation-qr.svg",
        ] {
            assert!(html.contains(expected), "{host}: missing {expected}");
        }
        assert!(!html.contains("https://cyberinjurylaw.com"), "{host}");
        assert!(!html.contains("20%"), "{host}");
        assert!(!html.contains("fonts.googleapis.com"), "{host}");
        assert!(!html.contains("fonts.gstatic.com"), "{host}");
        assert!(!html.contains("assets/app.js"), "{host}");
    }
}

/// The share-card contract: every public page advertises the full Open Graph
/// + Twitter Card set with absolute brand URLs, and one canonical address.
#[tokio::test]
async fn public_pages_emit_complete_open_graph_and_canonical_metadata() {
    // Canonical URLs and absolute social images always point at the brand's
    // canonical www host — a staging mirror canonicalizes to production.
    let origin = "https://www.cyberinjurylaw.com";
    for host in HOSTS {
        for (path, title, description) in [
            (
                "/",
                "CyberInjuryLaw | We use AI to put more money in your pocket",
                "Car accidents, negligence, medical malpractice, data theft, cybercrime. We investigate, preserve evidence, and go after whoever cut the corner. Tell us what happened. Attorney advertisement.",
            ),
            (
                "/services",
                "CyberInjuryLaw | Injury and cybercrime claims",
                "Car accidents, negligence, medical malpractice, data theft, cybercrime, and online abuse. Tell us what happened; a lawyer reviews the facts before any engagement. Attorney advertisement.",
            ),
        ] {
            let (status, html) = page(host, path).await;
            assert_eq!(status, StatusCode::OK, "{host}{path}");
            let url = format!("{origin}{path}");
            for tag in [
                format!(r#"content="{title}" property="og:title""#),
                r#"content="CyberInjuryLaw" property="og:site_name""#.to_string(),
                format!(r#"content="{description}" property="og:description""#),
                format!(r#"content="{url}" property="og:url""#),
                format!(
                    r#"content="{origin}/assets/img/cyber-injury-law/canal-street-ad-f9faa356.png" property="og:image""#
                ),
                r#"property="og:image:alt""#.to_string(),
                format!(r#"rel="canonical" href="{url}""#),
                r#"name="twitter:card" content="summary""#.to_string(),
                format!(r#"name="twitter:title" content="{title}""#),
                r#"name="twitter:description""#.to_string(),
                format!(
                    r#"name="twitter:image" content="{origin}/assets/img/cyber-injury-law/canal-street-ad-f9faa356.png""#
                ),
            ] {
                assert!(
                    html.contains(&tag),
                    "{host}{path}: missing social head fragment {tag}; emitted social metas: {}",
                    html
                        .split("<meta")
                        .filter(|m| m.contains("twitter") || m.contains("og:"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                );
            }
        }
    }
}

#[tokio::test]
async fn printed_consultation_url_redirects_to_the_brands_booking_provider() {
    for host in HOSTS {
        let state = portal::test_support::app_state(store::test_support::mem_surreal().await).await;
        let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/consultation?next=https://example.org")
                    .header("host", host)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT, "{host}");
        assert_eq!(
            response.headers()["location"],
            views::brand::CYBER_INJURY_BRANDING.consultation_url,
            "{host}"
        );
    }
}

/// The campaign reports to its own Plausible site.
#[test]
fn campaign_is_live_with_its_own_analytics() {
    assert!(BrandKey::CyberInjuryLaw.is_live());
    assert!(
        portal::plausible::PlausibleSite::for_brand(BrandKey::CyberInjuryLaw)
            .script_tags()
            .contains("pa-Ns-yAKpkSNXzfvnEJYZJX.js")
    );
}

#[tokio::test]
async fn cyber_injury_testimonials_are_public() {
    for host in HOSTS {
        let (status, _) = page(host, "/testimonials").await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
}

/// The campaign links back to DeleteYourData.com, the sibling practice that
/// helps a reader make their data harder to find before the next leak.
#[tokio::test]
async fn cyber_injury_home_links_to_delete_your_data() {
    for host in HOSTS {
        let (status, html) = page(host, "/").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for expected in [
            "sibling-practices",
            "MAKE YOUR DATA HARDER TO FIND.",
            "Visit DeleteYourData.com",
            "deleteyourdata.com\"",
            "$29/mo",
        ] {
            assert!(html.contains(expected), "{host} is missing {expected:?}");
        }
    }
}
