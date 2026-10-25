//! Cards pointing a reader from one practice's home to a sibling practice.
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One sibling-practice card with its destination already resolved for this
/// deployment. Mirrors `views::locales::SiblingPracticeCopy`, for the same
/// wasm-build reason [`super::EstateContent`] mirrors its catalog type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SiblingPractice {
    pub eyebrow: String,
    pub heading: String,
    pub body: String,
    pub cta_label: String,
    pub href: String,
    /// Why the offer matters, when the catalog says.
    #[serde(default)]
    pub note: String,
    /// `[citation, url]` pairs the note relies on.
    #[serde(default)]
    pub sources: Vec<[String; 2]>,
}

/// The cards, or nothing when there are none. Each brand's stylesheet skins
/// `.sibling-practices` under its own page root.
#[component]
pub(super) fn SiblingPractices(cards: Vec<SiblingPractice>) -> Element {
    if cards.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "sibling-practices", "aria-label": "Also from Guieb & Shook PC",
            for card in cards {
                article { class: "sibling-practice",
                    p { class: "sibling-practice__eyebrow", "{card.eyebrow}" }
                    h2 { "{card.heading}" }
                    p { "{card.body}" }
                    if !card.note.is_empty() {
                        p { class: "sibling-practice__note", "{card.note}" }
                    }
                    if !card.sources.is_empty() {
                        ul { class: "sibling-practice__sources", "aria-label": "Sources",
                            for [citation, url] in card.sources.clone() {
                                li {
                                    a {
                                        href: url,
                                        target: "_blank",
                                        rel: "noopener noreferrer",
                                        "{citation}"
                                    }
                                }
                            }
                        }
                    }
                    a {
                        class: "sibling-practice__link",
                        href: card.href.clone(),
                        target: "_blank",
                        rel: "noopener noreferrer",
                        "{card.cta_label} ↗"
                    }
                }
            }
        }
    }
}
