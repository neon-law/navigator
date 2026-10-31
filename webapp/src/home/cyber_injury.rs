//! `CyberInjuryLaw`'s approved campaign, rendered and hydrated by Dioxus.

mod assessment;
mod content;
pub use content::CyberInjuryContent;

use super::HomeContent;
use crate::components::{share::via_tagged, BrandFavicon, SocialMeta, THEME_STYLESHEET_HREF};
use crate::lead_capture::{LeadCaptureContext, LeadCaptureCopy};
use crate::public_chrome::{PublicChrome, PublicFooter};
use dioxus::prelude::*;

/// A discovery card's link, tagged for Plausible attribution: the tag
/// derives from the card's own page (`/car-accidents` →
/// `home-discovery-car-accidents`), so every card is attributable without a
/// hand-typed tag per card.
fn discovery_card_href(href: &str) -> String {
    via_tagged(
        href,
        &format!("home-discovery-{}", href.trim_start_matches('/')),
    )
}

#[component]
pub(super) fn CyberInjuryHome(
    chrome: PublicChrome,
    content: HomeContent,
    copy: CyberInjuryContent,
    #[props(default)] lead_capture: Option<LeadCaptureCopy>,
    #[props(default)] lead_context: LeadCaptureContext,
) -> Element {
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: content.meta_description.clone() }
        SocialMeta { title: content.head_title.clone(), description: content.meta_description.clone(), og_description: content.og_description.clone(), site_name: chrome.brand_name.clone(), image: chrome.social_image.clone(), url: crate::components::canonical_url(&chrome.canonical_origin, "/") }
        BrandFavicon { logo_href: chrome.logo_href.clone() }
        document::Stylesheet { href: THEME_STYLESHEET_HREF }
        document::Stylesheet { href: crate::brand_style::BRAND_STYLESHEET_HREF }
        document::Stylesheet { href: chrome.tokens_href.clone() }
        document::Stylesheet { href: "/public/css/cyber-injury-law.css" }
        document::Stylesheet { href: super::HOME_STYLESHEET_HREF }
        div { class: "cyber-page",
        div { class: "cyber-site",
            a { class: "cyber-skip", href: "#cyber-main", "{copy.skip_label}" }
            div { class: "cyber-utility", span { "{copy.utility_lines[0]}" } span { "{copy.utility_lines[1]}" } }
            header { class: "cyber-header",
                a { class: "cyber-wordmark", href: "/", "aria-label": "{copy.home_label}",
                    img { src: chrome.logo_href.clone(), alt: "", width: "44", height: "44" }
                    span { "{copy.header_wordmark[0]}" span { class: "cyber-lime", "{copy.header_wordmark[1]}" } "{copy.header_wordmark[2]}" small { "{copy.tagline}" } }
                }
                nav { "aria-label": "{copy.navigation_label}",
                    a { href: "#cyber-advantage", "{copy.advantage_label}" }
                    a { href: via_tagged("/testimonials", "home-nav-testimonials"), span { "aria-hidden": "true", "♡ " } "{copy.testimonials_label}" }
                    a { class: "cyber-button cyber-button--small", href: "#cyber-assessment", "{copy.assessment_label}" span { "aria-hidden": "true", "↗" } }
                }
                a { class: "cyber-booking", href: via_tagged("/consultation", "home-booking"), "aria-label": "{copy.booking_label}",
                    img { src: "/public/brand/cyber-consultation-qr.svg", alt: "{copy.booking_qr_alt}", width: "84", height: "84" }
                    span { "{copy.booking_lines[0]}" strong { "{copy.booking_lines[1]}" } small { "{copy.booking_lines[2]}" } }
                }
            }
            main { id: "cyber-main",
                section { class: "cyber-hero", "aria-labelledby": "cyber-title",
                    div { class: "cyber-hero__image",
                        img { src: copy.hero_src.clone(), alt: "{copy.portrait_alt}", fetchpriority: "high", width: "1536", height: "1024" }
                        p { "{copy.portrait_name}" small { "{copy.portrait_caption}" } }
                    }
                    div { class: "cyber-hero__copy",
                        p { class: "cyber-eyebrow", span { class: "cyber-lime", "aria-hidden": "true", "✳ " } "{copy.eyebrow}" }
                        h1 { id: "cyber-title",
                            for (index, line) in copy.hero_lines.iter().enumerate() {
                                span {
                                    if !copy.hero_highlights[index].is_empty() {
                                        if let Some(prefix) = line.strip_suffix(copy.hero_highlights[index].as_str()) {
                                            "{prefix}" span { class: "cyber-lime", "{copy.hero_highlights[index]}" }
                                        } else { "{line}" }
                                    } else { "{line}" }
                                }
                            }
                        }
                        p { class: "cyber-lead", "{content.lead}" }
                        a { class: "cyber-button", href: "#cyber-assessment", "{content.contact_label}" span { "aria-hidden": "true", "↗" } }
                        p { class: "cyber-hero__notes", "{copy.hero_summary}" }
                        p { class: "cyber-fine", "{copy.hero_note}" }
                    }
                }
                div { class: "cyber-ticker", span { "{copy.ticker_lines[0]}" } span { "aria-hidden": "true", "✳" } span { "{copy.ticker_lines[1]}" } span { "aria-hidden": "true", "✳" } span { "{copy.ticker_lines[2]}" } }
                section { id: "cyber-advantage", class: "cyber-section cyber-advantage",
                    div { class: "cyber-advantage__heading",
                        div { p { class: "cyber-eyebrow", "{copy.discovery_eyebrow}" } h2 { span { "{copy.discovery_heading[0]}" } span { class: "cyber-lime", "{copy.discovery_heading[1]}" } } }
                        p { "{copy.discovery_body}" }
                    }
                    div { class: "cyber-cards", for (index, card) in copy.discovery_cards.iter().enumerate() {
                        if card[3].is_empty() {
                            article { DiscoveryCardContent { index, title: card[0].clone(), body: card[1].clone(), benefit: card[2].clone() } }
                        } else {
                            a { class: "cyber-card-link", href: discovery_card_href(&card[3]),
                                article { DiscoveryCardContent { index, title: card[0].clone(), body: card[1].clone(), benefit: card[2].clone() } }
                            }
                        }
                    } }
                    p { class: "cyber-fine", "{copy.discovery_note}" }
                }
                section { id: "cyber-assessment", class: "cyber-section cyber-split cyber-paper",
                    div { class: "cyber-intro",
                        p { class: "cyber-eyebrow", "{copy.assessment_eyebrow}" }
                        h2 { for (index, line) in copy.assessment_heading.iter().enumerate() { span { class: if index == 0 { "" } else { "cyber-muted" }, "{line}" } } }
                        p { "{copy.assessment_body}" }
                        p { class: "cyber-privacy", "{copy.privacy_lines[0]}" br {} "{copy.privacy_lines[1]}" }
                    }
                    assessment::Assessment {
                        copy: copy.clone(),
                        consultation_href: content.contact_href,
                        lead_capture,
                        lead_context,
                    }
                }
                section { class: "cyber-closing",
                    p { class: "cyber-eyebrow", "{copy.closing_eyebrow}" }
                    h2 { span { "{copy.closing_lines[0]}" } span { "{copy.closing_lines[1]}" } }
                    a { class: "cyber-button cyber-button--dark", href: "#cyber-assessment", "{copy.closing_label}" span { "aria-hidden": "true", "↗" } }
                }
            }
            super::sibling_practice::SiblingPractices { cards: copy.sibling_practices.clone() }
            section { class: "cyber-connect", "aria-labelledby": "cyber-connect-title",
                div { class: "cyber-connect__intro",
                    h2 { id: "cyber-connect-title", "{copy.connect_heading}" }
                    a { class: "cyber-contact-phone", href: "{copy.phone_href}", "{copy.phone_label}" }
                    span { "{copy.email}" }
                }
                div { class: "cyber-connect__links",
                    for (index, (href, qr)) in [
                        (via_tagged("https://www.cyberinjurylaw.com/android", "home-qr-android"), "cyber-android-qr.svg"),
                        (via_tagged("https://www.cyberinjurylaw.com/iPhone", "home-qr-iphone"), "cyber-iphone-qr.svg"),
                        (copy.phone_href.clone(), "cyber-phone-qr.svg"),
                        (via_tagged("https://www.cyberinjurylaw.com/testimonials", "home-qr-testimonials"), "cyber-testimonials-qr.svg"),
                    ].into_iter().enumerate() {
                        div { class: "cyber-connect__item",
                            a { class: "cyber-connect__link", href,
                                img { src: "/public/brand/{qr}", alt: "{copy.qr_alts[index]}", width: "112", height: "112", loading: "lazy" }
                                strong { if index == 3 { span { "aria-hidden": "true", "♡ " } } "{copy.qr_labels[index]}" }
                                small { "{copy.qr_descriptions[index]}" }
                            }
                            a { class: "cyber-qr-download", href: "/public/brand/{qr}", download: qr, "{copy.qr_save_label}" }
                        }
                    }
                }
            }
            div { class: "cyber-campaign-footer",
                strong { "{copy.footer_wordmark[0]}" span { class: "cyber-yellow", "{copy.footer_wordmark[1]}" } "{copy.footer_wordmark[2]}" } span { "{copy.advertising_label}" }
                a { href: copy.ad_src, "{copy.ad_label}" }
                p { "{copy.footer_note}" }
            }
        }
        div { class: "nav-theme", PublicFooter { chrome: chrome.clone() } }
        }
    }
}

#[component]
fn CarIcon() -> Element {
    rsx! {
        svg {
            width: "44",
            height: "44",
            view_box: "0 0 48 48",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            // A collision scene makes the vehicle-claim category distinct
            // from a generic transportation or navigation icon.
            path { d: "M8 28l3-10a4 4 0 0 1 4-3h18a4 4 0 0 1 4 3l3 10v9h-4v-4H12v4H8v-9Z" }
            path { d: "M11 27h26M15 16l-2 9m20-9 2 9M15 30h.01M33 30h.01" }
            path { d: "m5 8 2 2m-5 4h4m2 4 2-2m34-8-2 2m5 4h-4m-2 4-2-2" }
        }
    }
}

/// One discovery card's content: the glyph follows the card's position in
/// the grid, and the title, body, and benefit label come from the catalog.
/// Linked cards wrap this in the anchor the grid styles.
#[component]
fn DiscoveryCardContent(index: usize, title: String, body: String, benefit: String) -> Element {
    rsx! {
        span { class: "cyber-glyph", "aria-hidden": "true",
            if index == 0 { CarIcon {} }
            else if index == 1 { "⌕" }
            else { "↗" }
        }
        h3 { "{title}" }
        p { "{body}" }
        span { class: "cyber-benefit", "{benefit}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyber_home_server_renders_the_offer_and_interactive_controls() {
        let html = dioxus_ssr::render_element(rsx! {
            CyberInjuryHome {
                chrome: PublicChrome::default(),
                content: HomeContent { heading: "We use AI to put more money in your pocket".into(), contact_href: "/contact".into(), ..HomeContent::default() },
                copy: CyberInjuryContent { hero_lines: ["WE USE AI TO".into(), "PUT MORE".into(), "MONEY IN".into(), "YOUR POCKET.".into()], hero_highlights: [String::new(), "MORE".into(), "MONEY IN".into(), "YOUR POCKET.".into()], advertising_label: "Attorney Advertising".into(), utility_lines: ["From YAML".into(), "Coast to Coast with Tech to Boast".into()], footer_wordmark: ["CYBER".into(), "INJURY".into(), "LAW".into()], ticker_lines: ["CAR ACCIDENTS.".into(), "PERSONAL INJURY.".into(), "CYBERCRIME.".into()], discovery_cards: vec![["CAR INJURY".into(), "Vehicle collision claims.".into(), "CAR ACCIDENTS".into(), "/car-accidents".into()], ["NEGLIGENCE".into(), "Carelessness claims.".into(), "CARELESSNESS".into(), String::new()]], incident_label: "A custom incident question".into(), hero_src: "/public/img/cyber-injury-law/cyber-warrior-ferris.png".into(), ..CyberInjuryContent::default() },
                lead_capture: Some(LeadCaptureCopy {
                    consent_sentence: "By sending this, you agree to our Privacy Policy.".into(),
                    phone_helper: "Optional. See Privacy Policy and texting terms.".into(),
                    sms_label: "Yes, send me text messages about this inquiry.".into(),
                }),
                lead_context: LeadCaptureContext {
                    csrf_token: "csrf-token".into(),
                    source_path: "/".into(),
                    ..LeadCaptureContext::default()
                },
            }
        });
        for required in [
            "WE USE AI TO",
            "From YAML",
            "Coast to Coast with Tech to Boast",
            "A custom incident question",
            "class=\"cyber-yellow\">INJURY</span>",
            "YOUR POCKET.",
            "cyber-incident",
            "cyber-timing",
            "PERSONAL INJURY.",
            "name=\"email\"",
            "name=\"phone\"",
            "name=\"sms_consent\"",
            "name=\"_csrf\"",
            "action=\"/leads\"",
            "Get in touch",
            "Attorney Advertising",
            "cyber-warrior-ferris.png",
            "<svg",
        ] {
            assert!(html.contains(required), "missing {required}");
        }
        for tagged in [
            "href=\"/testimonials?via=home-nav-testimonials\"",
            "href=\"/consultation?via=home-booking\"",
            "href=\"https://www.cyberinjurylaw.com/testimonials?via=home-qr-testimonials\"",
        ] {
            assert!(html.contains(tagged), "share refs missing {tagged}");
        }
        assert!(html.contains("class=\"cyber-lime\">MORE</span>"));
        assert!(html.contains("class=\"cyber-lime\">MONEY IN</span>"));
        assert!(html.contains("class=\"cyber-lime\">YOUR POCKET.</span>"));
        // A card with a page renders as a link tagged for Plausible; a
        // card without one stays a bare article.
        assert!(
            html.contains("href=\"/car-accidents?via=home-discovery-car-accidents\""),
            "linked discovery card missing its via tag"
        );
        assert!(!html.contains("20%"));
        assert!(!html.contains("cyber-fees"));
        assert!(!html.contains("fonts.googleapis.com"));
        assert!(!html.contains("<script"));
        assert!(!html.contains("dangerous_inner_html"));
        assert_eq!(
            html.matches("<form").count(),
            1,
            "CyberInjuryLaw should have a single form"
        );
        assert!(!html.contains("name=\"cyber-records\""));
    }

    #[test]
    fn discovery_card_links_carry_a_via_tag() {
        assert_eq!(
            discovery_card_href("/car-accidents"),
            "/car-accidents?via=home-discovery-car-accidents"
        );
    }
}
