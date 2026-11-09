use dioxus::prelude::*;

use crate::components::{tel_href, BrandFavicon, THEME_STYLESHEET_HREF};
use crate::public_chrome::PublicChrome;

const SUGGESTIONS: [&str; 3] = [
    "How do I start an LLC?",
    "What should I include in a will?",
    "How do I respond to a demand letter?",
];

#[component]
pub(super) fn SearchHome(chrome: PublicChrome, title: String) -> Element {
    let signed_in = chrome
        .utility
        .iter()
        .any(|link| link.href == "/auth/logout");
    let phone = tel_href(&chrome.firm_phone);
    rsx! {
        document::Title { "{title}" }
        BrandFavicon { logo_href: chrome.logo_href }
        document::Stylesheet { href: THEME_STYLESHEET_HREF }
        document::Stylesheet { href: crate::brand_style::BRAND_STYLESHEET_HREF }
        document::Stylesheet { href: "{chrome.tokens_href}" }
        document::Stylesheet { href: "/public/css/signup-prompt.css" }
        document::Stylesheet { href: "/public/css/nautilus-home.css" }
        document::Script { src: "/public/js/nautilus-home.js", defer: true }
        div { class: "nav-theme nautilus-home",
            nav { "aria-label": "Main navigation",
                a { href: "/app/nautilus", "Nautilus" }
                a { href: "/notations", "Notations" }
                a { href: "/navigator", "Navigator" }
            }
            main {
                form { id: "nautilus-search", role: "search",
                    h1 { label { r#for: "nautilus-question", "What’s your legal need?" } }
                    div { class: "nautilus-home__input",
                        input {
                            class: "nav-input",
                            id: "nautilus-question",
                            r#type: "search",
                            required: true,
                            maxlength: "2000",
                            autocomplete: "off",
                        }
                        button { r#type: "submit", class: "nav-btn nav-btn--primary", "Search" }
                    }
                    div { class: "nautilus-home__suggestions",
                        for suggestion in SUGGESTIONS {
                            button { r#type: "button", class: "nav-btn nav-btn--secondary",
                                "data-question": suggestion, "{suggestion}"
                            }
                        }
                    }
                }
                p { a { href: "{phone}", "Prefer to talk? Call Neon Law" } }
            }
            dialog { id: "nautilus-search-dialog", class: "signup-prompt",
                "aria-labelledby": "nautilus-dialog-title",
                "data-signed-in": if signed_in { "true" } else { "false" },
                h2 { id: "nautilus-dialog-title", "Search with Nautilus" }
                if signed_in {
                    p { "Your question is ready. Identity verification is required before we can run it." }
                    button { class: "nav-btn nav-btn--primary", r#type: "button", disabled: true, "Verify identity and search" }
                    p { "Online identity verification is coming soon. You can call us for help in the meantime." }
                } else {
                    p { "Sign in or create an account, then verify your identity to search. We’ll keep your question ready." }
                    a { class: "nav-btn nav-btn--primary", href: "/auth/login/oidc?return_to=/",
                        "Continue with Google"
                    }
                    a { class: "nav-btn nav-btn--secondary", href: "/auth/login/apple?return_to=/",
                        "Continue with Apple"
                    }
                }
                details {
                    summary { "Why do I need to verify my identity?" }
                    p { "Neon Law is a law firm. We verify your identity so we know who’s asking for help. If you ask us to represent you, we’ll also check for conflicts before taking on your matter." }
                }
                video { controls: true, preload: "none", "aria-label": "Nautilus explainer",
                    "data-source": "/public/videos/nautilus-explainer.mp4",
                }
                p { a { href: "{phone}", "Prefer to talk? Call Neon Law" } }
                form { method: "dialog",
                    button { class: "nav-btn nav-btn--secondary", "Back to my question" }
                }
            }
        }
    }
}
