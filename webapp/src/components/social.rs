#![allow(clippy::doc_markdown)]
//! Open Graph + Twitter Card meta tags, as a Dioxus component (issue #641,
//! Phase 2). The product names in these docs (iMessage, LinkedIn, …) are not
//! code, so the module opts out of the backtick lint.
//!
//! The successor to the `views::components::social`. The social-share
//! "preview card" partial: when a link to the site is pasted into iMessage,
//! Slack, X, LinkedIn, or Discord, those clients read these `<meta>` tags to
//! render a rich preview (the brand logo, the page title, a one-line message).
//! The tags live in `<head>`; [`document::Meta`] hoists them there during SSR.
//!
//! `og_description` is the page's YAML `og_description`, when set: social
//! copy can run punchier than the search-result `description`, which it
//! otherwise falls back to.
//!
//! `url` is the page's **absolute** URL, which the caller builds from the
//! brand's canonical origin ([`crate::public_chrome::PublicChrome::canonical_origin`])
//! plus its own path. It drives both `og:url` — scrapers treat a shared link
//! and its `og:url` as the same page, so a relative or absent one forks the
//! like/share counts — and the `<link rel="canonical">` search engines
//! read. Empty (pages that pass no URL) emits neither tag rather than a
//! wrong one.
//!
//! `image` is the **absolute** logo URL — scrapers drop relative `og:image`
//! URLs, so the caller resolves it against the site origin server-side (the same
//! `absolute_url` boundary the version used) and passes the result in.

use dioxus::prelude::*;

/// The line a share card previews: the YAML `og_description` when the page sets
/// one, else its `description`.
fn preview_description(description: String, og_description: Option<String>) -> String {
    og_description
        .filter(|og| !og.trim().is_empty())
        .unwrap_or(description)
}

/// The page's absolute URL: `origin` plus `path`, or empty when either is
/// unknown. A page rendered without a resolved brand origin (the wasm
/// client, a bare `PublicChrome::default()`) has no honest canonical URL
/// to advertise, so it advertises none.
pub(crate) fn canonical_url(origin: &str, path: &str) -> String {
    if origin.is_empty() || path.is_empty() {
        return String::new();
    }
    format!("{origin}{path}")
}

/// Render the Open Graph + Twitter Card `<meta>` block for one page.
#[component]
pub fn SocialMeta(
    title: String,
    description: String,
    site_name: String,
    image: String,
    #[props(default)] og_description: Option<String>,
    #[props(default)] url: String,
) -> Element {
    let image_alt = format!("{site_name} logo");
    let description = preview_description(description, og_description);
    rsx! {
        // Open Graph — Facebook, iMessage, Slack, LinkedIn, Discord.
        document::Meta { property: "og:type", content: "website" }
        document::Meta { property: "og:site_name", content: "{site_name}" }
        document::Meta { property: "og:title", content: "{title}" }
        document::Meta { property: "og:description", content: "{description}" }
        if !url.is_empty() {
            document::Meta { property: "og:url", content: "{url}" }
        }
        if !image.is_empty() {
            document::Meta { property: "og:image", content: "{image}" }
            document::Meta { property: "og:image:alt", content: "{image_alt}" }
        }
        if !url.is_empty() {
            // The same URL search engines canonicalize: one address per page,
            // shared by the share card and the crawler.
            document::Link { rel: "canonical", href: "{url}" }
        }
        // Twitter / X. `summary` renders the square logo as a small thumbnail;
        // the wide card expects a 1.91:1 banner, which a square mark would
        // letterbox.
        document::Meta { name: "twitter:card", content: "summary" }
        document::Meta { name: "twitter:title", content: "{title}" }
        document::Meta { name: "twitter:description", content: "{description}" }
        if !image.is_empty() {
            document::Meta { name: "twitter:image", content: "{image}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_wordmark_does_not_emit_social_image_metadata() {
        fn app() -> Element {
            rsx! {
                SocialMeta {
                    title: "Lawyer Shook".to_string(),
                    description: "The Guieb & Shook PC holding page.".to_string(),
                    site_name: "Lawyer Shook".to_string(),
                    image: String::new(),
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(
            !out.contains("og:image"),
            "no empty Open Graph image: {out}"
        );
        assert!(
            !out.contains("twitter:image"),
            "no empty Twitter image: {out}"
        );
    }

    #[test]
    fn an_og_description_overrides_the_meta_description_in_previews() {
        let meta = "The search-result line.".to_string();
        assert_eq!(
            preview_description(meta.clone(), Some("The share-card line.".to_string())),
            "The share-card line."
        );
        assert_eq!(preview_description(meta.clone(), None), meta);
        assert_eq!(
            preview_description(meta.clone(), Some("  ".to_string())),
            meta
        );
    }

    #[test]
    fn canonical_url_joins_origin_and_path_and_refuses_unknowns() {
        assert_eq!(
            canonical_url("https://www.cyberinjurylaw.com", "/services"),
            "https://www.cyberinjurylaw.com/services"
        );
        assert_eq!(
            canonical_url("https://www.cyberinjurylaw.com", "/"),
            "https://www.cyberinjurylaw.com/"
        );
        // No resolved origin (wasm client, bare chrome): no URL to advertise.
        assert_eq!(canonical_url("", "/services"), "");
        assert_eq!(canonical_url("https://www.cyberinjurylaw.com", ""), "");
    }

    #[test]
    fn a_page_with_a_url_emits_og_url_and_a_canonical_link() {
        fn app() -> Element {
            rsx! {
                SocialMeta {
                    title: "Home".to_string(),
                    description: "The home page.".to_string(),
                    site_name: "CyberInjuryLaw".to_string(),
                    image: String::new(),
                    url: "https://www.cyberinjurylaw.com/".to_string(),
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(
            out.contains(r#"content="https://www.cyberinjurylaw.com/" property="og:url""#),
            "og:url must carry the absolute page URL: {out}"
        );
        assert!(
            out.contains(r#"rel="canonical" href="https://www.cyberinjurylaw.com/""#),
            "the canonical link must carry the same URL: {out}"
        );
    }

    #[test]
    fn a_page_without_a_url_emits_no_og_url_and_no_canonical_link() {
        fn app() -> Element {
            rsx! {
                SocialMeta {
                    title: "Home".to_string(),
                    description: "The home page.".to_string(),
                    site_name: "CyberInjuryLaw".to_string(),
                    image: String::new(),
                }
            }
        }
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        let out = dioxus_ssr::render(&dom);
        assert!(!out.contains("og:url"), "{out}");
        assert!(!out.contains("canonical"), "{out}");
    }
}
