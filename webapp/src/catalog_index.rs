//! `/workshops`, `/presentations`, and `/notations` — the firm's material indexes.
//!
//! Categories share the public shell and material list. The portal pre-layer
//! injects each category's content. Notations is the searchable template
//! gallery alone; the format's story lives at `/notations/about`
//! ([`crate::notations_about`]).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    CatalogHero, PublicShell, SocialMeta, TestimonialCard, TestimonialSection,
    CATALOG_STYLESHEET_HREF,
};
use crate::public_chrome::{PublicChrome, PublicFooter, PublicHeader};

/// One material in an index — a workshop or a presentation. `eyebrow` is the
/// small uppercase line above the title, naming the audience the material is
/// written for.
///
/// `kind` and the two `category_*` fields are notation-only (LAW-53): the
/// template's own declared `kind:` frontmatter (via
/// `views::kind_catalog::declared_kind`, the same classifier `S103` runs) and
/// the category [`rules::kind::Kind::category`] groups it under. Empty for a
/// workshop or presentation, which carries no `kind:` at all — `Explore the
/// templates`' kind/category facet and gallery grouping render only when
/// `kind` is non-empty.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct CatalogMaterial {
    pub href: String,
    pub eyebrow: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub kind: String,
    /// The kind's title-cased display label (e.g. `"Letter"`), for the
    /// facet pill and the card's kind tag.
    #[serde(default)]
    pub kind_label: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub category_slug: String,
}

/// One category index's resolved content, built per request by the portal
/// pre-layer and injected for [`catalog_index_view`]. The wasm-safe carrier
/// across the server-function boundary.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct CatalogIndexContent {
    /// The category's heading, and the page title after the brand name.
    pub title: String,
    /// The companion page that explains the format, linked under the
    /// heading. Empty renders no link.
    #[serde(default)]
    pub about_href: String,
    /// The hero paragraph, reused as the page's meta description.
    pub lede: String,
    pub materials: Vec<CatalogMaterial>,
    /// The inbox the empty state writes to.
    pub contact_email: String,
    /// The line under the list. Empty renders nothing.
    pub footnote: String,
    /// Whether the catalog ends with the public testimonials section.
    #[serde(default)]
    pub include_testimonials: bool,
    /// The request's resolved brand, used only for the server-side store read.
    #[serde(default)]
    pub brand_key: String,
}

/// The [`CatalogIndexContent`] the portal pre-layer injects, read back in
/// [`catalog_index_view`].
#[derive(Clone, Default)]
pub struct InjectedCatalogIndex(pub CatalogIndexContent);

/// Everything the page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct CatalogIndexView {
    pub chrome: PublicChrome,
    pub content: CatalogIndexContent,
    #[serde(default)]
    pub testimonials: Vec<TestimonialCard>,
}

/// Resolve the shared chrome and this category's injected content.
#[server]
pub async fn catalog_index_view() -> Result<CatalogIndexView, ServerFnError> {
    let content = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedCatalogIndex>,
        _,
    >()
    .await
    .map(|axum::Extension(c)| c.0)
    .unwrap_or_default();
    let surreal = consume_context::<store::surreal::SurrealDb>();
    let testimonials = if content.include_testimonials {
        store::testimonials::published_for_brand(&surreal, &content.brand_key, Some(12))
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?
            .into_iter()
            .map(|testimonial| TestimonialCard {
                quote: testimonial.quote,
                attribution: testimonial.attribution_label.unwrap_or_default(),
                detail: None,
                profile_image_url: None,
                product_label: None,
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(CatalogIndexView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
        testimonials,
    })
}

/// The page's route entry, mounted once per category.
#[component]
pub fn CatalogIndexEntry() -> Element {
    let resource = use_server_future(catalog_index_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        CatalogIndexPage {
            chrome: view.chrome,
            content: view.content,
            testimonials: view.testimonials,
        }
    }
}

/// The pure index page. Prop-driven, so it server-renders and unit-tests
/// without a server future.
#[component]
pub fn CatalogIndexPage(
    chrome: PublicChrome,
    content: CatalogIndexContent,
    #[props(default)] testimonials: Vec<TestimonialCard>,
) -> Element {
    let header = rsx! {
        PublicHeader { chrome: chrome.clone() }
    };
    let footer = rsx! {
        PublicFooter { chrome: chrome.clone() }
    };
    let head_title = format!("{} | {}", chrome.brand_name, content.title);
    // A material with a `kind` is a notation: the catalog is then the
    // template gallery, with its own heading and stylesheet.
    let is_gallery = content.materials.iter().any(|m| !m.kind.is_empty());
    rsx! {
        document::Title { "{head_title}" }
        document::Meta { name: "description", content: content.lede.clone() }
        // The share card. Load-bearing on the presentations index in
        // particular: the talks are the firm's most-shared public surface, so
        // this is the preview every pasted link to a talk index renders.
        //
        // Not assertable from the unit tests below: `document::*` hoists into
        // `<head>` during the real SSR pipeline and never appears in
        // `dioxus_ssr::render` output. The covering test is the
        // `brand_routing.feature` scenario that greps `og:site_name` off `/`
        // through the real router.
        SocialMeta {
            title: head_title.clone(),
            description: content.lede.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        document::Stylesheet { href: CATALOG_STYLESHEET_HREF }
        PublicShell { header, footer,
            if is_gallery {
                document::Stylesheet { href: crate::notations_about::NOTATIONS_STYLESHEET_HREF }
            } else {
                CatalogHero {
                    eyebrow: chrome.brand_name.clone(),
                    title: content.title.clone(),
                    lede: content.lede.clone(),
                }
            }
            section { id: "templates", class: "catalog-library",
                class: if is_gallery { "catalog-library--notations" },
            if is_gallery {
                h1 { "{content.title}" }
            }
            if !content.about_href.is_empty() {
                p { class: "catalog-about",
                    a { href: "{content.about_href}", "About the Notation format →" }
                }
            }
            if content.materials.is_empty() {
                p { class: "catalog-empty",
                    "This catalog is still loading. Email "
                    "{content.contact_email}"
                    " for the runbook in the meantime."
                }
            } else {
                if is_gallery {
                    TemplateGallery { materials: content.materials.clone() }
                } else {
                    CatalogMaterialList { materials: content.materials.clone() }
                }
                if !content.footnote.is_empty() {
                    p { class: "catalog-more", "{content.footnote}" }
                }
            }
            }
            if content.include_testimonials {
                TestimonialSection {
                    heading: "Testimonials".to_string(),
                    lead: String::new(),
                    cards: testimonials,
                }
            }
        }
    }
}

/// The template gallery's category display order (LAW-53): correspondence
/// first — the letters most authors reach for — then the other
/// notation-family instruments, and the rarer families last. A category not
/// listed here sorts after every listed one, in first-seen order, so a
/// future category never disappears from the gallery silently.
const CATEGORY_ORDER: &[&str] = &[
    "correspondence",
    "instrument",
    "filing",
    "court-paper",
    "content",
    "engineering",
    "matter-dashboard",
];

/// Group `materials` by `category_slug`, in [`CATEGORY_ORDER`] order,
/// preserving each material's original relative order within its group. A
/// material with no `category_slug` (a workshop or presentation, which
/// carries no `kind:`) is dropped — this grouping exists only for the
/// notation gallery.
fn group_by_category(materials: &[CatalogMaterial]) -> Vec<(String, String, Vec<CatalogMaterial>)> {
    let mut groups: Vec<(String, String, Vec<CatalogMaterial>)> = Vec::new();
    for material in materials {
        if material.category_slug.is_empty() {
            continue;
        }
        match groups
            .iter_mut()
            .find(|(slug, _, _)| *slug == material.category_slug)
        {
            Some(group) => group.2.push(material.clone()),
            None => groups.push((
                material.category_slug.clone(),
                material.category.clone(),
                vec![material.clone()],
            )),
        }
    }
    groups.sort_by_key(|(slug, _, _)| {
        CATEGORY_ORDER
            .iter()
            .position(|s| s == slug)
            .unwrap_or(CATEGORY_ORDER.len())
    });
    groups
}

/// Every distinct `(kind, kind_label, count)` present in `materials`, in
/// first-seen order — the kind-facet pills' data.
fn distinct_kinds(materials: &[CatalogMaterial]) -> Vec<(String, String, usize)> {
    let mut kinds: Vec<(String, String, usize)> = Vec::new();
    for material in materials {
        if material.kind.is_empty() {
            continue;
        }
        match kinds.iter_mut().find(|(kind, _, _)| *kind == material.kind) {
            Some(entry) => entry.2 += 1,
            None => kinds.push((material.kind.clone(), material.kind_label.clone(), 1)),
        }
    }
    kinds
}

/// The kind-filter pills' CSS, generated per bundled kind rather than
/// shipped as a static rule so a new bundled template's kind filters for
/// free. Checking a kind's pill hides every gallery card whose `data-kind`
/// does not match it, and hides a category section left with no visible
/// card. The "All" pill needs no rule: it is the default state every other
/// rule only overrides while its own pill is checked.
fn kind_filter_style(kinds: &[(String, String, usize)]) -> String {
    use std::fmt::Write;
    let mut css = String::new();
    for (kind, _, _) in kinds {
        let _ = writeln!(
            css,
            "#kind-filter-{kind}:checked ~ .template-gallery [data-kind]:not([data-kind=\"{kind}\"]) {{ display: none; }}"
        );
        let _ = writeln!(
            css,
            "#kind-filter-{kind}:checked ~ .template-gallery .template-gallery__group:not(:has([data-kind=\"{kind}\"])) {{ display: none; }}"
        );
    }
    css
}

/// The template gallery (LAW-53): a grid of preview cards grouped by
/// category, with a kind facet above it. Renders only for a catalog whose
/// materials carry a `kind` (today, `/notations` alone) — `CatalogMaterialList`
/// still serves a catalog with none.
#[component]
fn TemplateGallery(materials: Vec<CatalogMaterial>) -> Element {
    let kinds = distinct_kinds(&materials);
    let groups = group_by_category(&materials);
    let total = materials.len();
    let style = kind_filter_style(&kinds);
    rsx! {
        document::Script { src: "/public/js/template-search.js", defer: true }
        div { class: "template-search", hidden: true,
            label { r#for: "template-search", "Search templates" }
            input {
                id: "template-search",
                class: "nav-template-search__input",
                r#type: "search",
                placeholder: "Title, form code, jurisdiction, or description",
                "aria-controls": "template-results",
            }
            p { id: "template-search-status", role: "status" }
        }
        style { "{style}" }
        div {
            class: "template-gallery-wrapper",
            role: "radiogroup",
            "aria-label": "Filter templates by kind",
            input {
                r#type: "radio",
                name: "kind-filter",
                id: "kind-filter-all",
                class: "nav-template-filter__input",
                checked: true,
            }
            label { r#for: "kind-filter-all", class: "template-filter__pill", "All ({total})" }
            for (kind, label, count) in kinds.iter() {
                input {
                    r#type: "radio",
                    name: "kind-filter",
                    id: "kind-filter-{kind}",
                    class: "nav-template-filter__input",
                }
                label {
                    r#for: "kind-filter-{kind}",
                    class: "template-filter__pill",
                    "{label} ({count})"
                }
            }
            div { class: "template-gallery", id: "template-results",
                for (slug, label, group_materials) in groups.iter() {
                    section {
                        class: "template-gallery__group",
                        key: "{slug}",
                        "data-category": "{slug}",
                        h2 { "{label}" }
                        ul { class: "template-gallery__cards",
                            for material in group_materials.iter() {
                                li {
                                    class: "template-card",
                                    key: "{material.href}",
                                    "data-kind": "{material.kind}",
                                    "data-template-search": "{material.title} {material.eyebrow} {material.summary} {material.href} {material.kind_label}",
                                    a { class: "template-card__link", href: "{material.href}",
                                        div { class: "template-card__body",
                                            p { class: "catalog-eyebrow", "{material.eyebrow}" }
                                            h3 { "{material.title}" }
                                            p { "{material.summary}" }
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

/// One list of materials.
#[component]
fn CatalogMaterialList(materials: Vec<CatalogMaterial>) -> Element {
    rsx! {
        ul { class: "catalog-materials",
            for material in materials.iter() {
                li { class: "catalog-material",
                    p { class: "catalog-eyebrow", "{material.eyebrow}" }
                    h3 {
                        a { href: "{material.href}", "{material.title}" }
                    }
                    p { "{material.summary}" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssr(app: fn() -> Element) -> String {
        let mut dom = VirtualDom::new(app);
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn material(title: &str, href: &str) -> CatalogMaterial {
        CatalogMaterial {
            href: href.to_string(),
            eyebrow: "For lawyers".to_string(),
            title: title.to_string(),
            summary: "What you take away.".to_string(),
            ..CatalogMaterial::default()
        }
    }

    /// A kind-tagged material, for the gallery/facet tests — the shape
    /// `notation_card` builds once a real `kind:` is derived from the
    /// template's own frontmatter (LAW-53).
    fn kind_material(
        kind: &str,
        kind_label: &str,
        category: &str,
        category_slug: &str,
        title: &str,
        href: &str,
    ) -> CatalogMaterial {
        CatalogMaterial {
            href: href.to_string(),
            eyebrow: kind_label.to_string(),
            title: title.to_string(),
            summary: "A bundled sample.".to_string(),
            kind: kind.to_string(),
            kind_label: kind_label.to_string(),
            category: category.to_string(),
            category_slug: category_slug.to_string(),
        }
    }

    fn workshops() -> CatalogIndexContent {
        CatalogIndexContent {
            title: "Workshops".to_string(),
            lede: "Hands-on classes.".to_string(),
            materials: vec![
                material("Using Neon Law Navigator", "/workshops/use-the-navigator"),
                material(
                    "Operating Neon Law Navigator",
                    "/workshops/deploy-the-navigator",
                ),
            ],
            contact_email: "support@example.org".to_string(),
            footnote: "More classes land here as we run them.".to_string(),
            include_testimonials: false,
            brand_key: String::new(),
            about_href: String::new(),
        }
    }

    fn html() -> String {
        fn app() -> Element {
            rsx! {
                CatalogIndexPage { chrome: PublicChrome::default(), content: workshops() }
            }
        }
        ssr(app)
    }

    #[test]
    fn the_heading_names_the_category() {
        let out = html();
        assert!(out.contains("Workshops"), "category heading: {out}");
    }

    #[test]
    fn each_material_links_to_its_own_page() {
        let out = html();
        assert!(
            out.contains(r#"href="/workshops/use-the-navigator""#),
            "first material href: {out}"
        );
        assert!(
            out.contains(r#"href="/workshops/deploy-the-navigator""#),
            "second material href: {out}"
        );
    }

    #[test]
    fn the_index_advertises_a_gated_class_it_cannot_open() {
        // The index is public while the material behind it is not: an
        // anonymous reader must still learn the class exists. Losing the
        // title or the summary here turns the gate into a dead end.
        let out = html();
        assert!(out.contains("Operating Neon Law Navigator"), "title: {out}");
        assert!(out.contains("What you take away."), "summary: {out}");
    }

    #[test]
    fn the_footnote_renders_under_the_list() {
        let out = html();
        assert!(
            out.contains("More classes land here as we run them."),
            "footnote: {out}"
        );
    }

    #[test]
    fn an_empty_category_offers_the_inbox_instead() {
        fn app() -> Element {
            let content = CatalogIndexContent {
                title: "Presentations".to_string(),
                contact_email: "support@example.org".to_string(),
                ..CatalogIndexContent::default()
            };
            rsx! {
                CatalogIndexPage { chrome: PublicChrome::default(), content }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains("support@example.org") && !out.contains("mailto:"),
            "the empty state must offer the inbox: {out}"
        );
    }

    #[test]
    fn notations_catalog_links_the_letters_and_a_form() {
        fn app() -> Element {
            let content = CatalogIndexContent {
                title: "Notations".to_string(),
                lede: "One markdown file is the template, questionnaire, and workflow.".to_string(),
                materials: vec![
                    CatalogMaterial {
                        href: "/notations/onboarding-letter".to_string(),
                        eyebrow: "Letter".to_string(),
                        title: "Onboarding Letter".to_string(),
                        summary: "Opens a matter.".to_string(),
                        ..CatalogMaterial::default()
                    },
                    CatalogMaterial {
                        href: "https://github.com/neon-law/navigator/blob/main/templates/notations/forms/united_states/nevada/state/nv__llc_formation.md".to_string(),
                        eyebrow: "Form · Nevada".to_string(),
                        title: "Nevada LLC Formation".to_string(),
                        summary: "Articles of organization.".to_string(),
                        ..CatalogMaterial::default()
                    },
                ],
                contact_email: "support@example.org".to_string(),
                footnote: String::new(),
                include_testimonials: false,
                brand_key: String::new(),
                about_href: String::new(),
            };
            rsx! {
                CatalogIndexPage { chrome: PublicChrome::default(), content }
            }
        }
        let out = ssr(app);
        assert!(out.contains("catalog-hero"), "catalog hero: {out}");
        assert!(out.contains("Onboarding Letter"), "letter title: {out}");
        assert!(
            out.contains(r#"href="/notations/onboarding-letter""#),
            "letter's default link opens the preview: {out}"
        );
        assert!(out.contains("nv__llc_formation.md"), "form href: {out}");
    }

    fn kind_tagged_materials() -> Vec<CatalogMaterial> {
        vec![
            kind_material(
                "onboarding",
                "Onboarding",
                "Correspondence",
                "correspondence",
                "Onboarding Letter",
                "/notations/onboarding-letter",
            ),
            kind_material(
                "letter",
                "Letter",
                "Correspondence",
                "correspondence",
                "Nevada Engagement Letter",
                "/notations/nevada-engagement-letter",
            ),
            kind_material(
                "filing",
                "Filing",
                "Filing",
                "filing",
                "Nevada LLC Formation",
                "/notations/nevada-llc-formation",
            ),
        ]
    }

    #[test]
    fn group_by_category_orders_groups_and_keeps_material_order_within_each() {
        let groups = group_by_category(&kind_tagged_materials());
        let slugs: Vec<&str> = groups.iter().map(|(slug, _, _)| slug.as_str()).collect();
        // `correspondence` precedes `filing` in `CATEGORY_ORDER`.
        assert_eq!(slugs, vec!["correspondence", "filing"]);
        let correspondence = &groups[0].2;
        assert_eq!(correspondence.len(), 2);
        assert_eq!(correspondence[0].title, "Onboarding Letter");
        assert_eq!(correspondence[1].title, "Nevada Engagement Letter");
    }

    #[test]
    fn distinct_kinds_counts_each_kind_once_in_first_seen_order() {
        let kinds = distinct_kinds(&kind_tagged_materials());
        assert_eq!(
            kinds,
            vec![
                ("onboarding".to_string(), "Onboarding".to_string(), 1),
                ("letter".to_string(), "Letter".to_string(), 1),
                ("filing".to_string(), "Filing".to_string(), 1),
            ]
        );
    }

    #[test]
    fn kind_filter_style_hides_non_matching_cards_and_emptied_groups() {
        let css = kind_filter_style(&distinct_kinds(&kind_tagged_materials()));
        assert!(css.contains("#kind-filter-onboarding:checked"), "{css}");
        assert!(
            css.contains(r#"[data-kind]:not([data-kind="onboarding"])"#),
            "{css}"
        );
        assert!(
            css.contains(":not(:has([data-kind=\"onboarding\"]))"),
            "{css}"
        );
        // "All" needs no generated rule — it is the unfiltered default state.
        assert!(!css.contains("kind-filter-all"), "{css}");
    }

    fn notations_content_with_kind_facets() -> CatalogIndexContent {
        CatalogIndexContent {
            title: "Notations".to_string(),
            lede: "Every kind, documented.".to_string(),
            materials: kind_tagged_materials(),
            contact_email: "support@example.org".to_string(),
            footnote: String::new(),
            include_testimonials: false,
            brand_key: String::new(),
            about_href: "/notations/about".to_string(),
        }
    }

    #[test]
    fn the_gallery_renders_a_kind_filter_pill_per_bundled_kind() {
        fn app() -> Element {
            rsx! {
                CatalogIndexPage {
                    chrome: PublicChrome::default(),
                    content: notations_content_with_kind_facets(),
                }
            }
        }
        let out = ssr(app);
        assert!(out.contains(r#"id="kind-filter-all""#), "all pill: {out}");
        assert!(
            out.contains(r#"id="kind-filter-onboarding""#),
            "onboarding pill: {out}"
        );
        assert!(
            out.contains(r#"for="kind-filter-onboarding""#),
            "onboarding pill label: {out}"
        );
    }

    #[test]
    fn the_gallery_groups_cards_by_category_with_a_heading_per_group() {
        fn app() -> Element {
            rsx! {
                CatalogIndexPage {
                    chrome: PublicChrome::default(),
                    content: notations_content_with_kind_facets(),
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains(r#"data-category="correspondence""#),
            "correspondence group: {out}"
        );
        assert!(
            out.contains(r#"data-category="filing""#),
            "filing group: {out}"
        );
        assert!(
            out.contains(r#"data-kind="onboarding""#),
            "card carries its kind: {out}"
        );
        assert!(
            out.contains(r#"href="/notations/nevada-llc-formation""#),
            "card still links to its preview: {out}"
        );
    }

    #[test]
    fn the_gallery_links_the_format_story_instead_of_telling_it() {
        fn app() -> Element {
            rsx! {
                CatalogIndexPage {
                    chrome: PublicChrome::default(),
                    content: notations_content_with_kind_facets(),
                }
            }
        }
        let out = ssr(app);
        assert!(
            out.contains(r#"href="/notations/about""#),
            "about link: {out}"
        );
        assert!(!out.contains("Every notation kind"), "{out}");
        assert!(!out.contains("notations-page"), "{out}");
        // A catalog without a companion page renders no link at all.
        assert!(!html().contains("catalog-about"), "{}", html());
    }
}
