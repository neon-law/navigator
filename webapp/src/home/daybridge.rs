//! Daybridge Divorce Law homepage.

use super::sibling_practice::{SiblingPractice, SiblingPractices};
use super::HomeContent;
use crate::components::{TestimonialCard, TestimonialSection};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// Copy loaded from the Daybridge home catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DaybridgeContent {
    pub testimonials_heading: String,
    pub signup_label: String,
    pub signin_label: String,
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
    rsx! {
        div { class: "daybridge-home",
            section { class: "daybridge-hero", "aria-labelledby": "daybridge-title",
                div {
                    h1 { id: "daybridge-title", "{content.heading}" }
                    p { class: "daybridge-lead", "{content.lead}" }
                    a {
                        class: "nav-btn nav-btn--primary daybridge-cta",
                        href: "{content.contact_href}",
                        "{content.contact_label}"
                    }
                    p { class: "daybridge-account",
                        a { href: SIGNUP_HREF, "{daybridge.signup_label}" }
                        a { href: SIGNIN_HREF, "{daybridge.signin_label}" }
                    }
                }
                img {
                    class: "daybridge-crossing",
                    src: "/public/brand/daybridge/crossing.svg",
                    alt: "",
                    width: "640",
                    height: "560",
                }
            }
            TestimonialSection {
                heading: daybridge.testimonials_heading.clone(),
                lead: String::new(),
                cards: testimonials,
            }
            SiblingPractices { cards: daybridge.sibling_practices.clone() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_contact_then_testimonials_before_the_sibling_practice() {
        fn app() -> Element {
            rsx! {
                DaybridgeHome {
                    content: HomeContent {
                        heading: "A way through divorce.".into(),
                        lead: "Move toward the other side.".into(),
                        contact_href: "/contact".into(),
                        contact_label: "Contact us to learn more".into(),
                        ..HomeContent::default()
                    },
                    daybridge: DaybridgeContent {
                        testimonials_heading: "What our clients say".into(),
                        signup_label: "Create your account".into(),
                        sibling_practices: vec![SiblingPractice {
                            heading: "After the divorce is final".into(),
                            href: "https://estate.example.com".into(),
                            ..SiblingPractice::default()
                        }],
                        ..DaybridgeContent::default()
                    },
                    testimonials: vec![TestimonialCard {
                        quote: "Synthetic testimonial".into(),
                        attribution: "Synthetic attribution".into(),
                        detail: None,
                        profile_image_url: None,
                        product_label: None,
                    }],
                }
            }
        }

        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let html = dioxus_ssr::render(&dom);
        let contact = html.find(r#"href="/contact""#).unwrap();
        let quote = html.find("Synthetic testimonial").unwrap();
        let sibling = html.find("https://estate.example.com").unwrap();
        assert!(contact < quote && quote < sibling, "{html}");
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
