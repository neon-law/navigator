//! `/free-legal-resources` — the firm's free court self-help form catalog.
//!
//! One index over the jurisdictions the court-form registry covers, plus one
//! page per jurisdiction grouping its courts and their forms. The rows are
//! not copy: the portal builds them straight from `store/seeds/Court.yaml`
//! and injects them here, so the page cannot drift from the registry — the
//! guard tests pin that join (`free_resources_pages_match_the_registry`).

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    CatalogHero, PublicShell, SiteHeader, SiteNavLink, SocialMeta, CATALOG_STYLESHEET_HREF,
};
use crate::public_chrome::{PublicChrome, PublicFooter};

/// One jurisdiction on the index — its display name, its catalog page, and
/// what the registry currently holds there.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourceJurisdictionSummary {
    /// The seeded display name (`Nevada`).
    pub name: String,
    /// The jurisdiction page's path (`/free-legal-resources/nevada`).
    pub href: String,
    pub court_count: usize,
    pub form_count: usize,
}

/// The index page's resolved content, built per request by the portal
/// pre-layer and injected for [`FreeResourcesIndexEntry`].
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourcesIndexContent {
    pub head_title: String,
    pub meta_description: String,
    pub title: String,
    pub lede: String,
    pub free_statement: String,
    pub no_outcome: String,
    pub jurisdictions: Vec<FreeResourceJurisdictionSummary>,
    pub footnote: String,
    /// The machine-readable endpoint's link label.
    pub json_label: String,
    /// The machine-readable endpoint's path (`/free-legal-resources/index.json`).
    pub json_href: String,
}

/// One form row on a jurisdiction page, already resolved from a registry
/// entry: the fee shown as the office publishes it, the fee waiver named by
/// its sibling row's title, and the blank pointed at the public assets
/// bucket — never a repository file.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourceFormRow {
    pub title: String,
    pub form_number: Option<String>,
    pub filing_office: String,
    /// The fee as displayed (`$270.00`), or the fee note prose when the
    /// schedule is tiered.
    pub fee: Option<String>,
    pub fee_waiver: Option<String>,
    pub source_url: String,
    pub blank_url: String,
}

/// One court on a jurisdiction page: its display name, where it sits, and
/// the form rows its filing offices accept.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourceCourt {
    pub name: String,
    /// The county or city and the court class (`Clark · District Court`).
    pub subtitle: String,
    pub forms: Vec<FreeResourceFormRow>,
}

/// A jurisdiction page's resolved content. `{jurisdiction}` placeholders in
/// the copy are already filled; the courts and their rows are the registry's.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourcesJurisdictionContent {
    pub head_title: String,
    pub meta_description: String,
    pub title: String,
    pub lede: String,
    pub free_statement: String,
    pub no_outcome: String,
    /// The jurisdiction's display name (`Nevada`) — the hero heading.
    pub jurisdiction_name: String,
    pub courts_heading: String,
    pub courts: Vec<FreeResourceCourt>,
    pub filing_office_label: String,
    pub fee_label: String,
    pub fee_waiver_label: String,
    pub source_label: String,
    pub blank_label: String,
    pub empty_court: String,
    pub empty_jurisdiction: String,
    pub footnote: String,
}

/// The [`FreeResourcesIndexContent`] the portal pre-layer injects, read back
/// in [`free_resources_index_view`]. The wasm-safe carrier across the
/// server-function boundary.
#[derive(Clone, Default)]
pub struct InjectedFreeResourcesIndex(pub FreeResourcesIndexContent);

/// The [`FreeResourcesJurisdictionContent`] the portal pre-layer injects, read
/// back in [`free_resources_jurisdiction_view`].
#[derive(Clone, Default)]
pub struct InjectedFreeResourcesJurisdiction(pub FreeResourcesJurisdictionContent);

/// Resolve the shared chrome and the injected index content.
#[server]
pub async fn free_resources_index_view() -> Result<FreeResourcesIndexView, ServerFnError> {
    let content = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedFreeResourcesIndex>,
        _,
    >()
    .await
    .map(|axum::Extension(c)| c.0)
    .unwrap_or_default();
    Ok(FreeResourcesIndexView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
    })
}

/// Everything the index page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourcesIndexView {
    pub chrome: PublicChrome,
    pub content: FreeResourcesIndexContent,
}

/// Resolve the shared chrome and the injected jurisdiction content.
#[server]
pub async fn free_resources_jurisdiction_view()
-> Result<FreeResourcesJurisdictionView, ServerFnError> {
    let content = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<InjectedFreeResourcesJurisdiction>,
        _,
    >()
    .await
    .map(|axum::Extension(c)| c.0)
    .unwrap_or_default();
    Ok(FreeResourcesJurisdictionView {
        chrome: crate::public_chrome::firm_public_chrome_from_context().await,
        content,
    })
}

/// Everything a jurisdiction page renders.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct FreeResourcesJurisdictionView {
    pub chrome: PublicChrome,
    pub content: FreeResourcesJurisdictionContent,
}

/// The index page's route entry, mounted once per host.
#[component]
pub fn FreeResourcesIndexEntry() -> Element {
    let resource = use_server_future(free_resources_index_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        FreeResourcesIndexPage {
            chrome: view.chrome,
            content: view.content,
        }
    }
}

/// The jurisdiction page's route entry.
#[component]
pub fn FreeResourcesJurisdictionEntry() -> Element {
    let resource = use_server_future(free_resources_jurisdiction_view)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        _ => return rsx! {},
    };
    rsx! {
        FreeResourcesJurisdictionPage {
            chrome: view.chrome,
            content: view.content,
        }
    }
}

fn shell_header(chrome: &PublicChrome) -> Element {
    rsx! {
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
    }
}

/// The promises every free-resources page carries, above its data.
#[component]
fn FreeResourcesPromises(free_statement: String, no_outcome: String) -> Element {
    rsx! {
        section { class: "catalog-free-promises",
            p { class: "catalog-free-promises__free", "{free_statement}" }
            p { class: "catalog-free-promises__no-outcome", "{no_outcome}" }
        }
    }
}

/// The pure index page. Prop-driven, so it server-renders and unit-tests
/// without a server future.
#[component]
pub fn FreeResourcesIndexPage(chrome: PublicChrome, content: FreeResourcesIndexContent) -> Element {
    let header = shell_header(&chrome);
    let footer = rsx! { PublicFooter { chrome: chrome.clone() } };
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: content.meta_description.clone() }
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        document::Stylesheet { href: CATALOG_STYLESHEET_HREF }
        PublicShell { header, footer,
            CatalogHero {
                eyebrow: chrome.brand_name.clone(),
                title: content.title.clone(),
                lede: content.lede.clone(),
            }
            FreeResourcesPromises {
                free_statement: content.free_statement.clone(),
                no_outcome: content.no_outcome.clone(),
            }
            section { id: "jurisdictions", class: "catalog-library",
                if content.jurisdictions.is_empty() {
                    p { class: "catalog-empty", "The court-form registry is loading." }
                } else {
                    ul { class: "catalog-materials",
                        for jurisdiction in content.jurisdictions.iter() {
                            li { class: "catalog-material", key: "{jurisdiction.href}",
                                h3 {
                                    a { href: "{jurisdiction.href}", "{jurisdiction.name}" }
                                }
                                p { "{jurisdiction.court_count} courts · {jurisdiction.form_count} forms" }
                            }
                        }
                    }
                    p { class: "catalog-more",
                        a { href: "{content.json_href}", "{content.json_label}" }
                    }
                }
                if !content.footnote.is_empty() {
                    p { class: "catalog-footnote", "{content.footnote}" }
                }
            }
        }
    }
}

/// The pure jurisdiction page: the registry's courts and rows, under the
/// copy's promises.
#[component]
pub fn FreeResourcesJurisdictionPage(
    chrome: PublicChrome,
    content: FreeResourcesJurisdictionContent,
) -> Element {
    let header = shell_header(&chrome);
    let footer = rsx! { PublicFooter { chrome: chrome.clone() } };
    rsx! {
        document::Title { "{content.head_title}" }
        document::Meta { name: "description", content: content.meta_description.clone() }
        SocialMeta {
            title: content.head_title.clone(),
            description: content.meta_description.clone(),
            site_name: chrome.brand_name.clone(),
            image: chrome.social_image.clone(),
        }
        document::Stylesheet { href: CATALOG_STYLESHEET_HREF }
        PublicShell { header, footer,
            CatalogHero {
                eyebrow: content.title.clone(),
                title: content.jurisdiction_name.clone(),
                lede: content.lede.clone(),
            }
            FreeResourcesPromises {
                free_statement: content.free_statement.clone(),
                no_outcome: content.no_outcome.clone(),
            }
            section { id: "courts", class: "catalog-library",
                h2 { "{content.courts_heading}" }
                if content.courts.is_empty() {
                    p { class: "catalog-empty", "{content.empty_jurisdiction}" }
                } else {
                    for court in content.courts.iter() {
                        div { class: "catalog-court", key: "{court.name}",
                            h3 { "{court.name}" }
                            if !court.subtitle.is_empty() {
                                p { class: "catalog-court__subtitle", "{court.subtitle}" }
                            }
                            if court.forms.is_empty() {
                                p { class: "catalog-empty", "{content.empty_court}" }
                            } else {
                                table { class: "catalog-forms-table",
                                    thead {
                                        tr {
                                            th { scope: "col", "{content.title}" }
                                            th { scope: "col", "{content.filing_office_label}" }
                                            th { scope: "col", "{content.fee_label}" }
                                            th { scope: "col", "{content.fee_waiver_label}" }
                                            th { scope: "col", "{content.source_label}" }
                                            th { scope: "col", "{content.blank_label}" }
                                        }
                                    }
                                    tbody {
                                        for row in court.forms.iter() {
                                            tr { key: "{row.title}",
                                                td {
                                                    span { class: "catalog-forms-table__title", "{row.title}" }
                                                    if let Some(number) = &row.form_number {
                                                        span { class: "catalog-forms-table__number", "{number}" }
                                                    }
                                                }
                                                td { "{row.filing_office}" }
                                                td {
                                                    if let Some(fee) = &row.fee {
                                                        "{fee}"
                                                    }
                                                }
                                                td {
                                                    if let Some(waiver) = &row.fee_waiver {
                                                        "{waiver}"
                                                    }
                                                }
                                                td {
                                                    a { href: "{row.source_url}", rel: "noopener", "{content.source_label}" }
                                                }
                                                td {
                                                    a { href: "{row.blank_url}", rel: "noopener", "{content.blank_label}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    p { class: "catalog-footnote", "{content.footnote}" }
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

    fn index() -> FreeResourcesIndexContent {
        FreeResourcesIndexContent {
            head_title: "Neon Law | Free Legal Resources".to_string(),
            meta_description: "Free court self-help forms for Nevada.".to_string(),
            title: "Free Legal Resources".to_string(),
            lede: "The practical facts around every form.".to_string(),
            free_statement: "Everything on these pages is free of charge.".to_string(),
            no_outcome: "These pages promise no outcome.".to_string(),
            jurisdictions: vec![FreeResourceJurisdictionSummary {
                name: "Nevada".to_string(),
                href: "/free-legal-resources/nevada".to_string(),
                court_count: 5,
                form_count: 2,
            }],
            footnote: "Links point to each court's official source.".to_string(),
            json_label: "Machine-readable JSON".to_string(),
            json_href: "/free-legal-resources/index.json".to_string(),
        }
    }

    fn jurisdiction() -> FreeResourcesJurisdictionContent {
        FreeResourcesJurisdictionContent {
            head_title: "Neon Law | Free Legal Resources".to_string(),
            meta_description: "Free court self-help forms for Nevada.".to_string(),
            title: "Free Legal Resources".to_string(),
            lede: "The practical facts around every form.".to_string(),
            free_statement: "Everything on these pages is free of charge.".to_string(),
            no_outcome: "These pages promise no outcome.".to_string(),
            jurisdiction_name: "Nevada".to_string(),
            courts_heading: "Courts and forms in Nevada".to_string(),
            courts: vec![FreeResourceCourt {
                name: "Eighth Judicial District Court".to_string(),
                subtitle: "Clark · District Court".to_string(),
                forms: vec![FreeResourceFormRow {
                    title: "Petition for Adult Name Change".to_string(),
                    form_number: None,
                    filing_office: "Family Division Clerk's Office".to_string(),
                    fee: Some("$270.00".to_string()),
                    fee_waiver: Some("Application to Waive Filing Fee".to_string()),
                    source_url: "https://selfhelp.nvcourts.gov/name-change".to_string(),
                    blank_url: "https://assets.example.org/forms/united_states/nevada/\
                                eighth_judicial_district_court/nv__adult_name_change_petition.pdf"
                        .to_string(),
                }],
            }],
            filing_office_label: "Filing office".to_string(),
            fee_label: "Filing fee".to_string(),
            fee_waiver_label: "Fee waiver".to_string(),
            source_label: "Official source".to_string(),
            blank_label: "Download blank".to_string(),
            empty_court: "This court has no forms in the registry yet.".to_string(),
            empty_jurisdiction: "No court in this jurisdiction has forms in the registry yet."
                .to_string(),
            footnote: "Links point to each court's official source.".to_string(),
        }
    }

    #[test]
    fn the_index_renders_each_jurisdiction_and_its_page_link() {
        let out = ssr(|| {
            rsx! {
                FreeResourcesIndexPage { chrome: PublicChrome::default(), content: index() }
            }
        });
        assert!(out.contains("Free Legal Resources"), "title: {out}");
        assert!(out.contains("Nevada"), "jurisdiction: {out}");
        assert!(
            out.contains(r#"href="/free-legal-resources/nevada""#),
            "jurisdiction link: {out}"
        );
        assert!(out.contains("5 courts · 2 forms"), "counts: {out}");
        assert!(
            out.contains(r#"href="/free-legal-resources/index.json""#),
            "JSON link: {out}"
        );
        assert!(out.contains("Everything on these pages is free of charge."), "free: {out}");
        assert!(out.contains("These pages promise no outcome."), "no outcome: {out}");
    }

    #[test]
    fn an_empty_index_renders_the_loading_state_not_a_blank_page() {
        let content = FreeResourcesIndexContent {
            jurisdictions: Vec::new(),
            ..index()
        };
        let out = ssr(|| {
            rsx! {
                FreeResourcesIndexPage { chrome: PublicChrome::default(), content }
            }
        });
        assert!(out.contains("The court-form registry is loading."), "{out}");
    }

    #[test]
    fn a_jurisdiction_page_renders_its_registry_rows() {
        let out = ssr(|| {
            rsx! {
                FreeResourcesJurisdictionPage { chrome: PublicChrome::default(), content: jurisdiction() }
            }
        });
        assert!(out.contains("Courts and forms in Nevada"), "heading: {out}");
        assert!(out.contains("Eighth Judicial District Court"), "court: {out}");
        assert!(out.contains("Clark · District Court"), "subtitle: {out}");
        assert!(out.contains("Petition for Adult Name Change"), "row: {out}");
        assert!(out.contains("Family Division Clerk's Office"), "office: {out}");
        assert!(out.contains("$270.00"), "fee: {out}");
        assert!(out.contains("Application to Waive Filing Fee"), "waiver: {out}");
        assert!(
            out.contains(r#"href="https://selfhelp.nvcourts.gov/name-change""#),
            "source link: {out}"
        );
        assert!(
            out.contains(r#"href="https://assets.example.org/forms/united_states/nevada/\
                            eighth_judicial_district_court/nv__adult_name_change_petition.pdf""#),
            "blank link: {out}"
        );
    }

    #[test]
    fn an_empty_court_states_its_registry_is_empty() {
        let mut content = jurisdiction();
        content.courts[0].forms.clear();
        let out = ssr(move || {
            rsx! {
                FreeResourcesJurisdictionPage { chrome: PublicChrome::default(), content }
            }
        });
        assert!(
            out.contains("This court has no forms in the registry yet."),
            "empty court state: {out}"
        );
    }

    #[test]
    fn an_empty_jurisdiction_states_its_registry_is_empty() {
        let mut content = jurisdiction();
        content.courts.clear();
        let out = ssr(move || {
            rsx! {
                FreeResourcesJurisdictionPage { chrome: PublicChrome::default(), content }
            }
        });
        assert!(
            out.contains("No court in this jurisdiction has forms in the registry yet."),
            "empty jurisdiction state: {out}"
        );
    }

    #[test]
    fn a_row_without_a_fee_or_waiver_leaves_those_cells_empty() {
        let mut content = jurisdiction();
        content.courts[0].forms[0].fee = None;
        content.courts[0].forms[0].fee_waiver = None;
        let out = ssr(move || {
            rsx! {
                FreeResourcesJurisdictionPage { chrome: PublicChrome::default(), content }
            }
        });
        assert!(!out.contains("$270.00"), "no fee cell: {out}");
        assert!(
            !out.contains("Application to Waive Filing Fee"),
            "no waiver cell: {out}"
        );
    }
}
