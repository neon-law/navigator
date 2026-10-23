//! Summons Defense: clear hearing fees and an official-source field guide.

use super::HomeContent;
use crate::components::{BrandFavicon, SocialMeta, THEME_STYLESHEET_HREF};
use crate::lead_capture::{LeadCaptureContext, LeadCaptureCopy, LeadCaptureForm};
use crate::public_chrome::{PublicChrome, PublicFooter};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Browser-safe copy resolved from the Summons catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SummonsContent {
    pub eyebrow: String,
    pub nav: Vec<[String; 2]>,
    pub hero_note: String,
    pub fees_heading: String,
    pub fees: Vec<[String; 4]>,
    pub fees_note: String,
    pub process_heading: String,
    pub steps: Vec<[String; 2]>,
    pub guides_heading: String,
    pub guides_intro: String,
    pub guides: Vec<[String; 5]>,
    pub cases_heading: String,
    pub cases_note: String,
    pub cases: Vec<[String; 4]>,
    pub resources_heading: String,
    pub resources: Vec<[String; 2]>,
    pub intake_heading: String,
    pub intake_body: String,
    pub intake_checklist: Vec<String>,
    pub intake_note: String,
    pub faq_heading: String,
    pub faqs: Vec<[String; 2]>,
    pub footer_note: String,
    pub reviewed: String,
}

#[component]
pub(super) fn SummonsHome(
    chrome: PublicChrome,
    content: HomeContent,
    copy: SummonsContent,
    lead_capture: Option<LeadCaptureCopy>,
    lead_context: LeadCaptureContext,
) -> Element {
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "viewport", content: "width=device-width, initial-scale=1" }
        document::Meta { name: "description", content: content.meta_description.clone() }
        SocialMeta { title: content.head_title.clone(), description: content.meta_description.clone(), og_description: content.og_description.clone(), site_name: chrome.brand_name.clone(), image: chrome.social_image.clone() }
        BrandFavicon { logo_href: chrome.logo_href.clone() }
        document::Stylesheet { href: THEME_STYLESHEET_HREF }
        document::Stylesheet { href: crate::brand_style::BRAND_STYLESHEET_HREF }
        document::Stylesheet { href: chrome.tokens_href.clone() }
        document::Stylesheet { href: "/public/css/summons.css" }
        div { class: "summons-page",
            a { class: "summons-skip", href: "#summons-main", "Skip to content" }
            div { class: "summons-notice", "{chrome.legal_entity} · Private legal representation · Attorney advertisement" }
            header { class: "summons-header summons-wrap",
                a { class: "summons-wordmark", href: "/", "aria-label": "Summons Defense home",
                    img { src: chrome.logo_href.clone(), alt: "", width: "42", height: "42" }
                    span { "SUMMONS" strong { "DEFENSE.NYC" } }
                }
                nav { "aria-label": "Main navigation",
                    for link in &copy.nav { a { href: "{link[1]}", "{link[0]}" } }
                }
                a { class: "summons-button summons-button--small", href: "#intake", "{content.contact_label}" span { "aria-hidden": "true", "↗" } }
            }
            main { id: "summons-main",
                section { class: "summons-hero summons-wrap", "aria-labelledby": "summons-title",
                    div { class: "summons-hero-copy",
                        p { class: "summons-eyebrow", span { class: "summons-dot", "aria-hidden": "true" } "{copy.eyebrow}" }
                        h1 { id: "summons-title", "{content.heading}" }
                        p { class: "summons-lead", "{content.lead}" }
                        a { class: "summons-button", href: "#intake", "{content.contact_label}" span { "aria-hidden": "true", "↗" } }
                        p { class: "summons-small", "{copy.hero_note}" }
                    }
                    aside { id: "fees", class: "summons-fees", "aria-labelledby": "summons-fees-title",
                        div { class: "summons-fee-top", p { class: "summons-eyebrow", id: "summons-fees-title", "{copy.fees_heading}" } span { "aria-hidden": "true", "↗" } }
                        for fee in copy.fees.iter() {
                            div { class: "summons-fee",
                                p { class: "summons-fee-label", "{fee[0]}" }
                                p { class: "summons-price", "{fee[1]}" span { "{fee[2]}" } }
                                p { "{fee[3]}" }
                            }
                        }
                        p { class: "summons-fee-note", "{copy.fees_note}" }
                    }
                }
                section { id: "how-it-works", class: "summons-process", "aria-labelledby": "summons-process-title",
                    div { class: "summons-wrap",
                        div { class: "summons-section-heading", p { class: "summons-eyebrow", "THE PROCESS" } h2 { id: "summons-process-title", "{copy.process_heading}" } }
                        ol { for step in copy.steps.iter() {
                            li { h3 { "{step[0]}" } p { "{step[1]}" } }
                        } }
                    }
                }
                section { id: "guides", class: "summons-section summons-wrap", "aria-labelledby": "summons-guides-title",
                    div { class: "summons-section-heading", p { class: "summons-eyebrow", "KNOW YOUR SUMMONS" } h2 { id: "summons-guides-title", "{copy.guides_heading}" } p { "{copy.guides_intro}" } }
                    div { class: "summons-guides", for guide in &copy.guides {
                        details { class: "summons-guide",
                            summary { span { class: "summons-agency", "{guide[0]}" } span { strong { "{guide[1]}" } small { "{guide[2]}" } } span { class: "summons-plus", "aria-hidden": "true", "+" } }
                            div { class: "summons-guide-body",
                                ul { for tip in guide[3].lines() { li { "{tip}" } } }
                                a { href: "{guide[4]}", "Official agency guidance ↗" }
                            }
                        }
                    } }
                }
                section { class: "summons-cases-band", "aria-labelledby": "summons-cases-title",
                    div { class: "summons-wrap summons-section",
                        div { class: "summons-section-heading", p { class: "summons-eyebrow", "FROM THE PUBLIC RECORD" } h2 { id: "summons-cases-title", "{copy.cases_heading}" } p { "{copy.cases_note}" } }
                        div { class: "summons-cases", for case in &copy.cases {
                            article { p { class: "summons-eyebrow", "{case[0]}" } h3 { "{case[1]}" } p { "{case[2]}" } a { href: "{case[3]}", "Read OATH’s case summary ↗" } }
                        } }
                    }
                }
                section { id: "intake", class: "summons-section summons-wrap summons-intake", "aria-labelledby": "summons-intake-title",
                    div { p { class: "summons-eyebrow", "LET’S GET STARTED" } h2 { id: "summons-intake-title", "{copy.intake_heading}" } p { "{copy.intake_body}" }
                        ul { for item in &copy.intake_checklist { li { "{item}" } } }
                    }
                    div { class: "summons-intake-form",
                        if let Some(lead_copy) = lead_capture {
                            div { class: "nav-theme", LeadCaptureForm { copy: lead_copy, context: lead_context } }
                        }
                        p { class: "summons-small", "{copy.intake_note}" }
                        a { href: "{content.contact_href}", "Book Consultation ↗" }
                    }
                }
                section { id: "resources", class: "summons-section summons-wrap summons-resources", "aria-labelledby": "summons-resources-title",
                    div { p { class: "summons-eyebrow", "THE OFFICIAL SOURCES" } h2 { id: "summons-resources-title", "{copy.resources_heading}" } p { class: "summons-small", "{copy.reviewed}" } }
                    ul { for resource in &copy.resources { li { a { href: "{resource[1]}", "{resource[0]}" span { "aria-hidden": "true", "↗" } } } } }
                }
                section { class: "summons-section summons-wrap summons-faq", "aria-labelledby": "summons-faq-title",
                    h2 { id: "summons-faq-title", "{copy.faq_heading}" }
                    div { for faq in &copy.faqs { details { summary { "{faq[0]}" } p { "{faq[1]}" } } } }
                }
                div { class: "summons-closing summons-wrap", p { "{copy.footer_note}" } a { class: "summons-button", href: "#intake", "{content.contact_label}" span { "aria-hidden": "true", "↗" } } }
            }
            div { class: "nav-theme", PublicFooter { chrome: chrome.clone() } }
        }
    }
}
