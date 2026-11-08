//! The host's legal documents — `/privacy` and `/terms` — as a Dioxus component
//! (#956 Phase 4).
//!
//! The successor to the `views::pages::{privacy, terms}` and the
//! `views::pages::policy` renderer they shared. Both are the same page: a title
//! over a `CommonMark` body that non-engineers edit in the deployment's own
//! `content/*.md` (e.g. `neon/content`) without touching Rust.
//! The portal router resolves the rendered body at construction and injects it,
//! so this component never parses markdown.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{PublicShell, SiteHeader, SiteNavLink};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// One legal document, resolved by the portal router at construction.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct LegalContent {
    /// The page heading and `<title>` suffix ("Privacy Policy").
    pub title: String,
    /// The `<meta name="description">`, named for the request's brand.
    pub description: String,
    /// The rendered HTML body (already sanitized; NOT raw markdown).
    pub body_html: String,
}

/// The [`LegalContent`] the portal router injects for this route.
#[derive(Clone, Default)]
pub struct InjectedLegal(pub LegalContent);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct LegalPageView {
    pub chrome: PublicChrome,
    pub content: LegalContent,
    /// The full document `<title>` ("Neon Law | Privacy Policy"), assembled from
    /// the brand and the document title.
    pub head_title: String,
}

/// Resolve the page: the injected document plus the host's public chrome.
#[server]
pub async fn get_legal_page() -> Result<LegalPageView, ServerFnError> {
    let content = consume_context::<InjectedLegal>().0;
    // Through the context helper, not `firm_public_chrome` directly: this
    // server-fn runs on a task that does not inherit the brand `task_local`, so
    // building the chrome here would read the default brand under a mounted
    // white-label bundle.
    let chrome = crate::public_chrome::firm_public_chrome_from_context().await;
    let head_title = format!("{} | {}", chrome.brand_name, content.title);
    let content = LegalContent {
        description: format!("{} for {}.", content.title, chrome.brand_name),
        body_html: firm_named_body(&content.body_html, &chrome),
        ..content
    };
    Ok(LegalPageView {
        chrome,
        content,
        head_title,
    })
}

/// The document as the request's brand publishes it.
///
/// The documents are written for Neon Law, the trade name of one Guieb & Shook
/// PC brand. Every other Guieb & Shook PC brand publishes them under the firm's
/// own name: the trade name becomes the legal entity, the clause defining it
/// and the Neon Law trademark section drop out, and the contact addresses are
/// the brand's own.
#[cfg(feature = "server")]
fn firm_named_body(body_html: &str, chrome: &PublicChrome) -> String {
    const TRADE_NAME: &str = "Neon Law";
    const FIRM: &str = "Guieb &amp; Shook PC";
    if chrome.legal_entity != "Guieb & Shook PC" || chrome.brand_name == TRADE_NAME {
        return body_html.to_string();
    }
    let body = match body_html.split_once("<h2>Trademarks</h2>") {
        Some((before, after)) => {
            let rest = after.find("<h2>").map_or("", |next| &after[next..]);
            format!("{before}{rest}")
        }
        None => body_html.to_string(),
    };
    body.replace(
        &format!("<strong>{TRADE_NAME}</strong> ({FIRM})"),
        &format!("<strong>{FIRM}</strong>"),
    )
    .replace(&format!("{TRADE_NAME} ({FIRM})"), FIRM)
    .replace(
        &format!(" Every reference to &quot;{TRADE_NAME}&quot; in these Terms means {FIRM}."),
        "",
    )
    .replace("contact@neonlaw.com", &chrome.firm_email)
    .replace("support@neonlaw.com", &chrome.firm_email)
    .replace(TRADE_NAME, FIRM)
}

/// The legal document page.
#[component]
pub fn LegalPage() -> Element {
    let resource = use_server_future(get_legal_page)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "legal", p { "Failed to load this document." } }
            }
        }
        None => {
            return rsx! {
                main { id: "legal", p { "Loading…" } }
            }
        }
    };
    legal_body(&view)
}

/// The loaded page. Split from the component so the tests render a fixed view
/// without standing up the server function.
fn legal_body(view: &LegalPageView) -> Element {
    let chrome = &view.chrome;
    let content = &view.content;
    let header = rsx! {
        SiteHeader {
            brand_name: chrome.brand_name.clone(),
            home_href: chrome.home_href.clone(),
            logo_href: chrome.logo_href.clone(),
            destinations: chrome
                .destinations
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
            utility: chrome
                .utility
                .iter()
                .map(|link| SiteNavLink::new(link.label.clone(), link.href.clone()))
                .collect(),
        }
    };
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };

    rsx! {
        document::Title { "{view.head_title}" }
        document::Meta { name: "description", content: "{content.description}" }
        PublicShell { header, footer,
            // A policy reads as a document: cap the measure so long clauses stay
            // readable, as the `article.policy` did.
            article { class: "policy", style: "max-width: 65ch; margin-inline: auto;",
                h1 { "{content.title}" }
                div { dangerous_inner_html: "{content.body_html}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(title: &str, body_html: &str) -> LegalPageView {
        LegalPageView {
            chrome: PublicChrome {
                brand_name: "Neon Law".to_string(),
                ..PublicChrome::default()
            },
            content: LegalContent {
                title: title.to_string(),
                description: format!("{title} for the Neon Law."),
                body_html: body_html.to_string(),
            },
            head_title: format!("Neon Law | {title}"),
        }
    }

    fn render(view: &LegalPageView) -> String {
        dioxus_ssr::render_element(legal_body(view))
    }

    #[test]
    fn the_document_leads_with_its_title() {
        let html = render(&view("Privacy Policy", "<p>Body.</p>"));
        assert!(html.contains("<h1>Privacy Policy</h1>"), "{html}");
        // `document::Title` / `document::Meta` are head elements the fullstack
        // head collector emits, not body markup, so the `<title>` and the
        // description are asserted against the real route instead — see
        // `server/tests/legal_pages.rs`.
    }

    #[test]
    fn the_rendered_markdown_body_is_emitted_as_html_not_escaped() {
        // The body is already-rendered, already-sanitized HTML from
        // `views::markdown` — escaping it here would show the tags as text.
        let html = render(&view(
            "Terms of Service",
            "<h2>First</h2>\n<p>First body.</p>",
        ));
        assert!(html.contains("<h2>First</h2>"), "{html}");
        assert!(html.contains("First body."), "{html}");
    }

    #[test]
    fn the_page_wears_the_public_shell_not_the_lawyer_chrome() {
        // These are anonymous-visitor pages; the lawyer nav must not leak in.
        let html = render(&view("Privacy Policy", "<p>Body.</p>"));
        assert!(html.contains("class=\"policy\""), "{html}");
        assert!(!html.contains("lawyer-nav"), "{html}");
    }
}
