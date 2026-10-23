//! Monthly and annual privacy plans and a decorative cloak crossing the internet.
use super::sibling_practice::{SiblingPractice, SiblingPractices};
use super::HomeContent;
use crate::components::is_external_href;
use crate::components::pricing::{PricingCard, PricingSection};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Client-renderable copy mapped from the server-only catalog schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PrivacyContent {
    pub eyebrow: String,
    pub price: String,
    pub price_term: String,
    pub annual_price: String,
    pub annual_term: String,
    pub monthly_label: String,
    pub annual_label: String,
    /// Billing details stated under the monthly price.
    #[serde(default)]
    pub household_note: String,
    pub annual_household_note: String,
    pub offer_note: String,
    pub gift_link: String,
    pub benefits_heading: String,
    pub record_label: String,
    pub record_heading: String,
    pub record_body: String,
    pub gift_heading: String,
    pub gift_body: String,
    pub gift_cta: String,
    pub gift_card_label: String,
    pub gift_card_term: String,
    pub closing_heading: String,
    pub benefits: Vec<[String; 2]>,
    /// Sibling-practice cards, each with its destination already resolved
    /// for this deployment. Empty when none is live.
    #[serde(default)]
    pub sibling_practices: Vec<SiblingPractice>,
}

#[component]
pub(super) fn PrivacyHome(content: HomeContent, privacy: PrivacyContent) -> Element {
    let booking_href = content.contact_href.clone();
    let booking_external = is_external_href(&booking_href);
    let cards = [
        (
            &privacy.monthly_label,
            &privacy.price,
            &privacy.price_term,
            &privacy.household_note,
        ),
        (
            &privacy.annual_label,
            &privacy.annual_price,
            &privacy.annual_term,
            &privacy.annual_household_note,
        ),
    ]
    .into_iter()
    .map(|(title, price, cadence, billing_note)| PricingCard {
        title: title.clone(),
        price: price.clone(),
        cadence: (!cadence.is_empty()).then(|| cadence.clone()),
        blurb: billing_note.clone(),
        features: Vec::new(),
        cta_label: content.contact_label.clone(),
        cta_href: booking_href.clone(),
        featured_label: Some(title.clone()),
        day_rate: None,
    })
    .collect();
    rsx! {
        div { class: "privacy-home",
            section { class: "privacy-hero", "aria-labelledby": "privacy-title",
                div {
                    p { class: "privacy-eyebrow", "{privacy.eyebrow}" }
                    h1 { id: "privacy-title", "{content.heading}" }
                }
                div { class: "privacy-offer",
                    p { "{content.lead}" }
                    PricingSection { cards, cols_lg: 2 }
                    div { class: "privacy-actions",
                        a { class: "privacy-link", href: "#gift", "{privacy.gift_link}" }
                    }
                    p { class: "privacy-small", "{privacy.offer_note}" }
                }
            }
            section { class: "privacy-exhibition",
                div { class: "privacy-scene", "aria-hidden": "true",
                    div { class: "privacy-network" }
                    div { class: "privacy-trail" }
                    for index in 0..6 {
                        div { class: "privacy-node privacy-node--{index}" }
                    }
                    Cloak {}
                }
            }
            section { class: "privacy-benefits", "aria-labelledby": "privacy-benefits-title",
                h2 { id: "privacy-benefits-title", "{privacy.benefits_heading}" }
                div { class: "privacy-benefits__grid",
                    for benefit in &privacy.benefits {
                        article { h3 { "{benefit[0]}" } p { "{benefit[1]}" } }
                    }
                }
            }
            section { class: "privacy-record", "aria-labelledby": "privacy-record-title",
                div {
                    p { class: "privacy-eyebrow", "{privacy.record_label}" }
                    div { class: "privacy-record__mark", "aria-hidden": "true", span {} span {} span {} }
                }
                div {
                    h2 { id: "privacy-record-title", "{privacy.record_heading}" }
                    p { "{privacy.record_body}" }
                }
            }
            SiblingPractices { cards: privacy.sibling_practices.clone() }
            section { id: "gift", class: "privacy-gift", "aria-labelledby": "privacy-gift-title",
                div { class: "privacy-giftcard", "aria-hidden": "true",
                    span { "{privacy.gift_card_label}" }
                    strong { "{privacy.price}" }
                    small { "{privacy.gift_card_term}" }
                }
                div {
                    p { class: "privacy-eyebrow", "{privacy.gift_link}" }
                    h2 { id: "privacy-gift-title", "{privacy.gift_heading}" }
                    p { "{privacy.gift_body}" }
                    a {
                        class: "privacy-link",
                        href: booking_href.clone(),
                        target: if booking_external { Some("_blank") } else { None },
                        rel: if booking_external { Some("noopener noreferrer") } else { None },
                        "{privacy.gift_cta} ↗"
                    }
                }
            }
            section { class: "privacy-closing",
                h2 { "{privacy.closing_heading}" }
                a {
                    class: "nav-btn nav-btn--primary",
                    href: booking_href,
                    target: if booking_external { Some("_blank") } else { None },
                    rel: if booking_external { Some("noopener noreferrer") } else { None },
                    "{content.contact_label} ↗"
                }
            }
        }
    }
}

/// A hood and flowing translucent fabric. The whole scene is decorative.
#[component]
fn Cloak() -> Element {
    rsx! {
        div { class: "privacy-cloak",
            svg { view_box: "0 0 240 280", fill: "none",
                defs {
                    linearGradient { id: "cloak-fabric", x1: "25", y1: "50", x2: "205", y2: "260", gradient_units: "userSpaceOnUse",
                        stop { stop_color: "#ffe4e4", stop_opacity: ".9" }
                        stop { offset: ".34", stop_color: "#ef4444", stop_opacity: ".55" }
                        stop { offset: ".68", stop_color: "#581626", stop_opacity: ".9" }
                        stop { offset: "1", stop_color: "#ef4444", stop_opacity: ".18" }
                    }
                }
                g { class: "privacy-cloak__fabric",
                    path { d: "M85 78 C62 102 59 167 20 239 C49 237 49 263 86 250 C107 274 125 250 148 263 C178 249 196 269 223 243 C184 193 184 128 154 79 Z", fill: "url(#cloak-fabric)", stroke: "#ffa3a3", stroke_width: "1.2" }
                    path { d: "M90 87 C91 144 61 215 59 246 M113 89 C108 161 96 208 106 257 M134 89 C134 150 161 215 150 260 M150 91 C164 158 175 215 194 253", stroke: "#ffc5c5", stroke_opacity: ".4", stroke_width: "1" }
                    path { d: "M69 91 C67 22 109 4 127 9 C155 15 177 50 169 96 C144 113 96 111 69 91Z", fill: "url(#cloak-fabric)", stroke: "#ffa3a3", stroke_width: "1.5" }
                    path { d: "M84 87 C82 49 108 29 121 27 C140 38 155 62 154 89 C135 102 105 103 84 87Z", fill: "#1b0910", stroke: "#ef4444", stroke_opacity: ".6" }
                    path { d: "M91 104 Q119 116 148 103", stroke: "#ffe6e6", stroke_width: "2" }
                }
            }
        }
    }
}
