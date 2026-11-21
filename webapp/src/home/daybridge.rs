//! Daybridge Divorce Law: a crossing at sunrise, two plain prices, and one way to start.

use super::sibling_practice::{SiblingPractice, SiblingPractices};
use super::HomeContent;
use crate::components::{is_external_href, TestimonialCard, TestimonialSection};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Copy loaded from the Daybridge home catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DaybridgeContent {
    pub eyebrow: String,
    pub fee_label: String,
    pub agreed_label: String,
    pub agreed_price: String,
    pub agreed_unit: String,
    pub agreed_body: String,
    pub contested_label: String,
    pub contested_price: String,
    pub contested_unit: String,
    pub contested_body: String,
    pub testimonials_heading: String,
    pub signup_label: String,
    pub signin_label: String,
    pub closing_heading: String,
    pub closing_body: String,
    #[serde(default)]
    pub sibling_practices: Vec<SiblingPractice>,
}

/// Where "Create your account" lands a new client: their matters.
const SIGNUP_HREF: &str = "/auth/signup?return_to=/app/projects";
const SIGNIN_HREF: &str = "/auth/login?return_to=/app/projects";

#[component]
pub(super) fn DaybridgeHome(
    content: HomeContent,
    daybridge: DaybridgeContent,
    #[props(default)] testimonials: Vec<TestimonialCard>,
) -> Element {
    let contact_external = is_external_href(&content.contact_href);
    rsx! {
        div { class: "daybridge-home",
            section { class: "daybridge-hero", "aria-labelledby": "daybridge-title",
                div { class: "daybridge-hero__copy",
                    p { class: "daybridge-eyebrow", "{daybridge.eyebrow}" }
                    h1 { id: "daybridge-title", "{content.heading}" }
                    p { class: "daybridge-lead", "{content.lead}" }
                    super::HeroCall { content: content.clone() }
                    a {
                        class: "{content.cta_class()} daybridge-cta",
                        href: "{content.contact_href}",
                        target: if contact_external { Some("_blank") } else { None },
                        rel: if contact_external { Some("noopener noreferrer") } else { None },
                        "{content.contact_label}"
                    }
                    a { class: "nav-btn nav-btn--secondary daybridge-cta", href: SIGNUP_HREF,
                        "{daybridge.signup_label}"
                    }
                    p { class: "daybridge-signin",
                        a { href: SIGNIN_HREF, "{daybridge.signin_label}" }
                    }
                }
                aside { class: "daybridge-fee", "aria-label": "{daybridge.fee_label}",
                    img {
                        class: "daybridge-fee__crossing",
                        src: "/public/brand/daybridge/crossing.svg",
                        alt: "",
                        width: "640",
                        height: "560",
                    }
                }
            }
            section { class: "daybridge-prices", "aria-label": "{daybridge.fee_label}",
                div {
                    p { class: "daybridge-eyebrow", "{daybridge.agreed_label}" }
                    p { class: "daybridge-price",
                        "{daybridge.agreed_price}"
                        span { "{daybridge.agreed_unit}" }
                    }
                    p { "{daybridge.agreed_body}" }
                }
                div {
                    p { class: "daybridge-eyebrow", "{daybridge.contested_label}" }
                    p { class: "daybridge-price",
                        "{daybridge.contested_price}"
                        span { "{daybridge.contested_unit}" }
                    }
                    p { "{daybridge.contested_body}" }
                }
            }
            TestimonialSection {
                heading: daybridge.testimonials_heading.clone(),
                lead: String::new(),
                cards: testimonials,
            }
            SiblingPractices { cards: daybridge.sibling_practices.clone() }
            section { class: "daybridge-closing", "aria-labelledby": "daybridge-closing-title",
                div {
                    h2 { id: "daybridge-closing-title", "{daybridge.closing_heading}" }
                    p { "{daybridge.closing_body}" }
                }
                div { class: "daybridge-closing__actions",
                    super::HeroCall { content: content.clone() }
                    a {
                        class: "{content.cta_class()} daybridge-cta",
                        href: "{content.contact_href}",
                        target: if contact_external { Some("_blank") } else { None },
                        rel: if contact_external { Some("noopener noreferrer") } else { None },
                        "{content.contact_label}"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_crossing_the_fee_and_the_sibling_practice() {
        fn app() -> Element {
            rsx! {
                DaybridgeHome {
                    content: HomeContent {
                        heading: "A way through divorce.".into(),
                        lead: "Move toward the other side.".into(),
                        contact_href: "mailto:contact@example.com".into(),
                        contact_label: "Start a conversation".into(),
                        ..HomeContent::default()
                    },
                    daybridge: DaybridgeContent {
                        fee_label: "Attorney fee".into(),
                        agreed_price: "$5,000".into(),
                        agreed_unit: " one-time fee".into(),
                        contested_price: "$9,000".into(),
                        signup_label: "Create your account".into(),
                        sibling_practices: vec![SiblingPractice {
                            heading: "After the divorce is final".into(),
                            href: "https://estate.example.com".into(),
                            ..SiblingPractice::default()
                        }],
                        ..DaybridgeContent::default()
                    },
                }
            }
        }

        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        assert!(html.contains("$5,000"), "{html}");
        assert!(html.contains("$9,000"), "{html}");
        assert!(html.contains("one-time fee"), "{html}");
        assert!(
            html.contains(r#"href="/auth/signup?return_to=/app/projects""#),
            "{html}"
        );
        assert!(!html.contains("<ol") && !html.contains("<li"), "{html}");
        assert!(
            html.contains("/public/brand/daybridge/crossing.svg"),
            "{html}"
        );
        assert!(
            html.contains(r#"href="https://estate.example.com""#),
            "{html}"
        );
    }
}
