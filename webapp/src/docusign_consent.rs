//! The static landing page for `DocuSign`'s JWT-grant consent callback.
//!
//! The callback is an inline `portal` handler rather than a Dioxus route, so
//! this module renders a small Dioxus component through the same standalone
//! document shell as the error pages.

use axum::response::Html;
use dioxus::prelude::*;

use crate::components::PublicShell;
use crate::public_chrome::{firm_public_chrome, PublicFooter, PublicHeader};

/// Render the confirmation shown after an operator grants `DocuSign` consent.
pub fn render() -> Html<String> {
    let body = dioxus_ssr::render_element(consent_body());
    Html(crate::error_pages::standalone_document(
        "DocuSign consent recorded",
        views::brand::FIRM_BRAND.site_name,
        &body,
    ))
}

fn consent_body() -> Element {
    let chrome = firm_public_chrome(vec![]);
    let header = rsx! {
        PublicHeader { chrome: chrome.clone() }
    };
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };
    rsx! {
        PublicShell { header, footer,
            section { class: "error-page",
                h1 { "Consent recorded" }
                p {
                    "DocuSign consent for the Neon Law Navigator integration has been granted. "
                    "You can close this tab — JWT grant does not use the redirect, so no "
                    "further action is needed here."
                }
                p { "The server can now mint access tokens for this account." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_a_complete_non_indexable_consent_document() {
        let html = render().0;
        assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
        assert!(html.contains("DocuSign consent recorded"), "{html}");
        assert!(html.contains("Consent recorded"), "{html}");
        assert!(
            html.contains("JWT grant does not use the redirect"),
            "{html}"
        );
        assert!(html.contains("public-shell"), "{html}");
        assert!(
            html.contains(r#"<meta name="robots" content="noindex">"#),
            "{html}"
        );
    }
}
