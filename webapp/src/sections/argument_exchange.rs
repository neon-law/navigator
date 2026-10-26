//! Ordered argument turns authored in a verified trial-preparation Notation.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One argument turn and the authority the lawyer identifies for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentTurn {
    pub text: String,
    pub authority: String,
}

/// One point, expected response, and reply in authored order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArgumentExchange {
    pub title: String,
    pub point: ArgumentTurn,
    pub response: ArgumentTurn,
    pub reply: ArgumentTurn,
}

/// Display data for the lawyer's argument-exchange section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentExchangeView {
    pub exchanges: Vec<ArgumentExchange>,
    pub unavailable: bool,
}

/// Render plain text, preserving multiline turns without interpreting HTML.
#[component]
pub fn ArgumentExchangeSection(view: ArgumentExchangeView) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "argument_exchange",
            aria_labelledby: "section-argument-exchange",
            crate::components::Card {
                header: rsx! { h2 { id: "section-argument-exchange", "Argument exchanges" } },
                if view.unavailable {
                    p { role: "alert", "Argument exchanges could not be loaded. Check the trial-preparation Notation." }
                } else if view.exchanges.is_empty() {
                    p { class: "matter-section__empty", "No argument exchanges are recorded yet." }
                } else {
                    ol { class: "matter-section__rows",
                        for exchange in &view.exchanges {
                            li { class: "matter-section__row",
                                article {
                                    h3 { "{exchange.title}" }
                                    dl {
                                        for (label, turn) in [("Our point", &exchange.point), ("Expected response", &exchange.response), ("Our reply", &exchange.reply)] {
                                            dt { strong { "{label}" } }
                                            dd {
                                                p { style: "white-space: pre-wrap", "{turn.text}" }
                                                p { style: "white-space: pre-wrap", strong { "Authority: " } "{turn.authority}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
