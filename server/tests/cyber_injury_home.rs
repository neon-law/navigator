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
            "href=\"/testimonials?via=home-nav-testimonials\"",
            // The QR card links the absolute testimonials URL; the share
            // reference (`?via=home-qr-testimonials`) rides along since the
            // share-tag convention landed on every site-built link.
            "href=\"https://www.cyberinjurylaw.com/testimonials?via=home-qr-testimonials\"",
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
                "We go after insurers that lowball you, companies that leak your data, and platforms that blame you. We investigate, preserve evidence, and file when necessary. A lawyer reviews the facts before any engagement. Attorney advertisement.",
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

/// Nautilus is the only privacy-plan card: the retired DeleteYourData.com
/// sibling card (the same product under its former name) must not resurface
/// on the home page.
#[tokio::test]
async fn cyber_injury_home_no_longer_links_to_delete_your_data() {
    for host in HOSTS {
        let (status, html) = page(host, "/").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for banned in ["DeleteYourData.com", "MAKE YOUR DATA HARDER TO FIND."] {
            assert!(
                !html.contains(banned),
                "{host} still carries the retired DeleteYourData card copy {banned:?}"
            );
        }
    }
}

/// Each practice page answers on the brand's own hosts with its per-page
/// share identity (the catalog's `head_title` and `og_description` reach the
/// page as `og:title` and `og:description`; the statement itself renders
/// word-by-word, so the raw HTML carries no contiguous headline), the
/// repeated consultation CTA, the phone number, straight answers, and its
/// helping-photo alt.
#[tokio::test]
async fn cyber_injury_practice_pages_serve_their_fight() {
    for host in HOSTS {
        for (path, og_title, share_line, alt) in [
            (
                "/car-accidents",
                "CyberInjuryLaw | Car accident lawyers",
                "We move before evidence disappears",
                "Two people sitting together at a kitchen table",
            ),
            (
                "/cybercrime",
                "CyberInjuryLaw | Cybercrime lawyers",
                "We preserve the evidence",
                "A man in a suit talking with a couple across a desk",
            ),
            (
                "/theft",
                "CyberInjuryLaw | Theft claims",
                "We preserve the proof and we pursue",
                "A man in a suit talking with a couple across a desk",
            ),
            (
                "/data-theft",
                "CyberInjuryLaw | Data theft lawyers",
                "We demand the records and put the blame where it belongs",
                "Hands organizing folders and papers across a desk",
            ),
        ] {
            let (status, html) = page(host, path).await;
            assert_eq!(status, StatusCode::OK, "{host}{path}");
            for expected in [
                og_title,
                share_line,
                "Tell us what happened",
                "/consultation",
                "tel:+15108002090",
                "1.510.800.2090",
                "Straight answers.",
                alt,
            ] {
                assert!(
                    html.contains(expected),
                    "{host}{path} is missing {expected:?}"
                );
            }
        }
    }
}

/// The Nautilus panel travels with the brand: the home page's sibling card
/// and the data-theft page's plan band both name the plan, its price and
/// scope, the non-legal-service note, and the link to Neon's hosted page.
#[tokio::test]
async fn cyber_injury_home_and_data_theft_link_the_nautilus_plan() {
    for host in HOSTS {
        let (status, home) = page(host, "/").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for expected in [
            "PROTECT YOUR PRIVACY FIRST.",
            "$29/mo",
            "$290/year prepaid",
            "dark-web monitoring",
            "identity-theft insurance",
            "non-legal service",
            "Visit the Nautilus plan",
            "neonlaw.com/nautilus",
        ] {
            assert!(
                home.contains(expected),
                "{host}: home is missing {expected:?}"
            );
        }

        let (status, data_theft) = page(host, "/data-theft").await;
        assert_eq!(status, StatusCode::OK, "{host}/data-theft");
        for expected in [
            "Make your data harder to use before you need us.",
            "The Nautilus plan",
            "$29/mo",
            "$290/year prepaid",
            "dark-web monitoring",
            "identity-theft insurance",
            "non-legal service",
            "Visit the Nautilus plan",
            "https://www.neonlaw.com/nautilus",
        ] {
            assert!(
                data_theft.contains(expected),
                "{host}/data-theft is missing {expected:?}"
            );
        }
    }
}

/// The home page's "Matters we review" cards link to the practice pages
/// they name, with the share-reference tag the campaign traces in Plausible.
#[tokio::test]
async fn cyber_injury_home_cards_link_their_practice_pages() {
    for host in HOSTS {
        let (status, html) = page(host, "/").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for path in ["/car-accidents", "/cybercrime", "/theft", "/data-theft"] {
            assert!(
                html.contains(&format!("href=\"{path}?via=")),
                "{host}: the home cards must link {path} with a via tag"
            );
        }
    }
}

/// Every practice path the Cyber Injury Law sitemap advertises is a page
/// its host serves.
#[tokio::test]
async fn cyber_injury_sitemap_advertises_its_practice_pages() {
    for host in HOSTS {
        let (status, xml) = page(host, "/sitemap.xml").await;
        assert_eq!(status, StatusCode::OK, "{host}");
        for path in ["/car-accidents", "/cybercrime", "/theft", "/data-theft"] {
            let url = format!("https://{host}{path}");
            assert!(xml.contains(&url), "{host}: sitemap is missing {url}");
        }
    }
}

/// A practice path answers only on the brand that declares it: the Neon
/// Law host 404s every Cyber Injury Law practice page.
#[tokio::test]
async fn cyber_injury_practice_paths_do_not_answer_on_the_neon_host() {
    for path in ["/car-accidents", "/cybercrime", "/theft", "/data-theft"] {
        let (status, _) = page("www.neonlaw.com", path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} on www.neonlaw.com");
    }
}
