//! `/notations/about` — the Notation format explained, in four slides.
//!
//! The `/notations` catalog is the list a reader came for; this page is where
//! the format's story lives: the specimen, the source-to-output diagram, the
//! four parts of a file, and the rule engine, followed by the generated kind
//! catalog (LAW-53) folded into a disclosure. The portal pre-layer injects the
//! content; the page itself is prop-driven so it server-renders and unit-tests
//! without a server future.
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    Accordion, BackBreadcrumb, PlainCodeBlock, PublicShell, SocialMeta, CATALOG_STYLESHEET_HREF,
};
use crate::marketing_page::PageContent;
use crate::public_chrome::{PublicChrome, PublicFooter, PublicHeader};

/// The stylesheet the format story and the kind catalog share with the
/// `/notations` gallery.
pub const NOTATIONS_STYLESHEET_HREF: &str = "/public/css/notations.css";

/// One kind-specific structural lint rule bound to a [`KindCatalogEntry`], as
/// a `(code, one-sentence requirement)` pair.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct KindRule {
    pub code: String,
    pub note: String,
}

/// One `kind:` value's catalog entry (LAW-53) — its definition, its category,
/// and the structural rules bound to it. Built server-side from
/// `views::kind_catalog::entries()`, itself a projection of
/// `rules::kind::Kind` — the same enum `S103` validates a declared `kind:`
/// against — so this catalog cannot drift from the gate.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct KindCatalogEntry {
    pub kind: String,
    pub label: String,
    pub category: String,
    pub category_slug: String,
    pub definition: String,
    pub rules: Vec<KindRule>,
}

/// The page's resolved content, built once by the host and injected per
/// request. The wasm-safe carrier across the server-function boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct NotationsAboutContent {
    /// The format story: hero copy and the marketing bands.
    pub introduction: PageContent,
    /// Every `kind:` value `S103` accepts (LAW-53).
    #[serde(default)]
    pub kinds: Vec<KindCatalogEntry>,
}

/// The [`NotationsAboutContent`] the portal pre-layer injects, read back in
/// [`notations_about_view`].
#[derive(Clone, Default)]
pub struct InjectedNotationsAbout(pub NotationsAboutContent);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct NotationsAboutView {
    pub chrome: PublicChrome,
    pub content: NotationsAboutContent,
}

/// Resolve the shared chrome and the injected content.
#[server]
pub async fn notations_about_view() -> Result<NotationsAboutView, ServerFnError> {
    let content = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedNotationsAbout>,
        _,
    >()
    .await
    .map(|axum::Extension(c)| c.0)
    .unwrap_or_default();
    Ok(NotationsAboutView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
    })
}

/// The page's route entry.
#[component]
pub fn NotationsAboutEntry() -> Element {
    let resource = use_server_future(notations_about_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        NotationsAboutPage { chrome: view.chrome, content: view.content }
    }
}

/// The pure page: the format story, then the kind catalog behind a
/// disclosure so the page reads as its four slides.
#[component]
pub fn NotationsAboutPage(chrome: PublicChrome, content: NotationsAboutContent) -> Element {
    let header = rsx! {
        PublicHeader { chrome: chrome.clone() }
    };
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };
    let head_title = format!("{} | About Notations", chrome.brand_name);
    let description = content.introduction.meta_description.clone();
    rsx! {
        document::Title { "{head_title}" }
        document::Meta { name: "description", content: description.clone() }
        SocialMeta {
            title: head_title.clone(),
            description,
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        document::Stylesheet { href: CATALOG_STYLESHEET_HREF }
        document::Stylesheet { href: NOTATIONS_STYLESHEET_HREF }
        PublicShell { header, footer,
            div { class: "notations-about",
                BackBreadcrumb { href: "/notations".to_string(), label: "Notations".to_string() }
            }
            NotationsIntroduction { content: content.introduction }
            if !content.kinds.is_empty() {
                div { class: "kind-catalog",
                    Accordion { title: "Every notation kind".to_string(),
                        KindCatalogSection { kinds: content.kinds.clone() }
                    }
                }
            }
        }
    }
}

/// The format's opening specimen, followed by the shared marketing bands.
#[component]
fn NotationsIntroduction(content: PageContent) -> Element {
    let specimen = "---\nkind: agreement\ntitle: Sample agreement\nprompts:\n  delivery: How should notices arrive?\nchoices:\n  delivery:\n    email: By email\n    post: By post\nquestionnaire:\n  BEGIN: { _: custom_single_choice__delivery }\n  custom_single_choice__delivery: { _: END }\n  END: {}\n# … workflow and other metadata\n---\n\n## I. Notices\n\nDelivery method: {{custom_single_choice__delivery}}.";
    rsx! {
        document::Stylesheet { href: crate::marketing_page::MARKETING_STYLESHEET_HREF }
        div { class: "notations-page",
            section { class: "notations-hero",
                div { class: "notations-hero__copy",
                    h1 { "{content.tagline}" }
                    div { class: "notations-lead",
                        crate::marketing_page::Prose { runs: content.hero_lead_runs.clone() }
                    }
                    div { class: "notations-actions",
                        if let Some(cta) = &content.hero_cta {
                            a { class: "nav-btn nav-btn--primary", href: "{cta.href}", "{cta.label} ↗" }
                        }
                        a { href: "#notation-flow", "Follow the format ↓" }
                    }
                }
                figure { class: "notations-specimen", id: "notation-source",
                    figcaption { span { "agreement.md" } }
                    PlainCodeBlock { code: specimen.to_string() }
                    div { class: "notations-specimen__footer",
                        span { "YAML Frontmatter" }
                        span { "+" }
                        span { "Markdown body" }
                    }
                }
            }
            crate::marketing_page::Bands { items: content.bands, notation_mark: true }
        }
    }
}

/// The kind catalog (LAW-53): every `kind:` value `S103` accepts, generated
/// from [`rules::kind::Kind`] — the same enum `S103` validates against — so
/// it can never drift from the gate.
#[component]
fn KindCatalogSection(kinds: Vec<KindCatalogEntry>) -> Element {
    rsx! {
        section { id: "kinds",
            p { class: "kind-catalog__intro",
                "Every value S103 accepts, generated from the same source the gate validates \
                 against — a definition, its category, and the structural rules bound to it."
            }
            ul { class: "kind-catalog__list",
                for entry in kinds.iter() {
                    li {
                        class: "kind-catalog__entry",
                        key: "{entry.kind}",
                        id: "kind-{entry.kind}",
                        "data-category": "{entry.category_slug}",
                        div { class: "kind-catalog__heading",
                            code { class: "kind-catalog__value", "{entry.kind}" }
                            span { class: "kind-catalog__category catalog-badge", "{entry.category}" }
                        }
                        p { class: "kind-catalog__definition", "{entry.definition}" }
                        if entry.rules.is_empty() {
                            p { class: "kind-catalog__rules kind-catalog__rules--none",
                                "No kind-specific structural rule beyond the shared Markdown \
                                 checks every file gets, regardless of kind."
                            }
                        } else {
                            ul { class: "kind-catalog__rules",
                                for rule in entry.rules.iter() {
                                    li { key: "{rule.code}",
                                        strong { "{rule.code}" }
                                        " — {rule.note}"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketing_page::{Band, HeroCta};

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn content() -> NotationsAboutContent {
        NotationsAboutContent {
            introduction: PageContent {
                meta_description: "Markdown and Frontmatter, explained.".to_string(),
                tagline: "Clear words. Working documents.".to_string(),
                hero_cta: Some(HeroCta {
                    href: "/notations".to_string(),
                    label: "Explore the templates".to_string(),
                }),
                bands: vec![Band::Cards {
                    anchor: "rules".to_string(),
                    overline: String::new(),
                    heading: "Plain text. Precise checks.".to_string(),
                    description: None,
                    items: Vec::new(),
                    pricing_style: false,
                }],
                ..PageContent::default()
            },
            kinds: vec![KindCatalogEntry {
                kind: "agreement".to_string(),
                label: "Agreement".to_string(),
                category: "Instrument".to_string(),
                category_slug: "instrument".to_string(),
                definition: "A private agreement (employment, contractor, LLC operating)"
                    .to_string(),
                rules: vec![KindRule {
                    code: "N123".to_string(),
                    note: "Depth-1 sections must be numbered with Roman numerals.".to_string(),
                }],
            }],
        }
    }

    fn html() -> String {
        fn app() -> Element {
            rsx! {
                NotationsAboutPage { chrome: PublicChrome::default(), content: content() }
            }
        }
        ssr(app)
    }

    #[test]
    fn the_story_leads_with_the_specimen_and_sends_readers_back_to_the_catalog() {
        let out = html();
        assert!(
            out.contains("<h1>Clear words. Working documents.</h1>"),
            "{out}"
        );
        assert!(out.contains(r#"id="notation-source""#), "specimen: {out}");
        assert!(out.contains(r#"id="rules""#), "band anchor: {out}");
        assert!(
            out.contains(r#"href="/notations""#),
            "back to the catalog: {out}"
        );
        assert!(out.contains("Explore the templates"), "catalog cta: {out}");
    }

    #[test]
    fn the_kind_catalog_documents_a_kind_with_its_rule_behind_a_disclosure() {
        let out = html();
        assert!(out.contains("<details"), "disclosure: {out}");
        assert!(out.contains("Every notation kind"), "{out}");
        assert!(out.contains(r#"id="kind-agreement""#), "{out}");
        assert!(
            out.contains("A private agreement (employment, contractor, LLC operating)"),
            "{out}"
        );
        assert!(out.contains("N123"), "{out}");
        assert!(
            out.contains("Depth-1 sections must be numbered with Roman numerals."),
            "{out}"
        );
    }

    #[test]
    fn a_page_with_no_kinds_renders_no_kind_catalog() {
        fn app() -> Element {
            let content = NotationsAboutContent {
                kinds: Vec::new(),
                ..content()
            };
            rsx! {
                NotationsAboutPage { chrome: PublicChrome::default(), content }
            }
        }
        let out = ssr(app);
        assert!(!out.contains("Every notation kind"), "{out}");
    }
}
