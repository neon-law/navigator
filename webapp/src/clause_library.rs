//! The lawyer-tier Clauses pages: `/app/admin/clauses` and `/app/admin/clauses/{code}`.
//!
//! A read-only view of the Clause library (docs/clauses.md). The list shows the
//! current version of every Clause with the number of Templates that pin it,
//! and narrows by code, kind, or wording. The detail page shows every version's
//! body and inputs and the Templates that pin that version. Wording changes
//! stay in the library YAML as a new version; nothing here writes.
//!
//! The catalog comes from `store::clauses::catalog`, the one read path the CLI
//! shares. Usage is read from each shared Template's compiled `clause_sources:`
//! frontmatter (`store::seed::seeded_clause_sources` for a seeded body stored
//! without it), so a Project's own Templates never appear here.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::people::ViewerRole;

/// The list page.
pub const CLAUSES_PATH: &str = "/app/admin/clauses";
/// The detail page for one Clause code.
pub const CLAUSE_PATH: &str = "/app/admin/clauses/{code}";

/// The list page's URL contract: `?q=` searches, `?kind=` filters.
#[derive(Deserialize, Default)]
pub struct ClauseQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// Where one Clause's detail page lives.
#[must_use]
pub fn clause_href(code: &str) -> String {
    format!("{CLAUSES_PATH}/{code}")
}

/// A Template that pins a Clause version.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct PinningTemplate {
    pub code: String,
    pub title: String,
}

/// One row of the list: a Clause's current version.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct ClauseRow {
    pub code: String,
    pub version: i32,
    pub kind: String,
    pub inputs: usize,
    /// Templates pinning any version of this code.
    pub templates: usize,
}

/// A Clause input, in a wasm-safe shape.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct InputView {
    pub name: String,
    pub types: Vec<String>,
    pub prompt: String,
}

/// One stored version of a Clause with the Templates that pin it.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct VersionView {
    pub version: i32,
    pub kind: String,
    pub is_current: bool,
    pub inputs: Vec<InputView>,
    pub body: String,
    pub templates: Vec<PinningTemplate>,
}

/// The page chrome every state of both pages carries.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct Chrome {
    pub role: ViewerRole,
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    #[serde(default)]
    pub tokens_href: String,
    #[serde(default)]
    pub firm_name: String,
}

/// The list page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct ClauseLibraryView {
    pub chrome: Chrome,
    pub query: String,
    pub kind: String,
    /// Every kind in the library, for the filter.
    pub kinds: Vec<String>,
    /// Current Clauses in the library before narrowing.
    pub total: usize,
    pub rows: Vec<ClauseRow>,
    pub failed: bool,
}

/// The detail page.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default, Debug)]
pub struct ClauseDetailView {
    pub chrome: Chrome,
    pub code: String,
    /// Newest version first. Empty when no such Clause exists.
    pub versions: Vec<VersionView>,
    pub failed: bool,
}

/// Whether a Clause matches the search `query` and `kind` filter. The query is
/// a case-insensitive substring of the code, the kind, or the body; an empty
/// query or kind matches everything.
#[must_use]
pub fn matches(code: &str, kind: &str, body: &str, query: &str, kind_filter: &str) -> bool {
    let kind_filter = kind_filter.trim();
    if !kind_filter.is_empty() && kind != kind_filter {
        return false;
    }
    let needle = query.trim().to_lowercase();
    needle.is_empty()
        || [code, kind, body]
            .iter()
            .any(|field| field.to_lowercase().contains(&needle))
}

#[cfg(feature = "server")]
mod load {
    use std::collections::BTreeMap;

    use dioxus::prelude::*;

    use super::{Chrome, PinningTemplate};
    use crate::people::ViewerRole;

    /// Refuse a non-lawyer caller, then gather the page chrome.
    pub async fn chrome() -> Result<Chrome, ServerFnError> {
        let role: ViewerRole = crate::admin_listing::require_lawyer().await?;
        Ok(Chrome {
            role,
            logo: crate::app_chrome::app_logo_from_context().await,
            tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
            firm_name: crate::app_chrome::firm_name_from_context().await,
        })
    }

    /// The shared Templates that pin each `code@version`, from their compiled
    /// `clause_sources:` frontmatter, or for a canonically seeded Template whose
    /// stored body omits it, from the bundled source it was seeded from. A
    /// Template whose body cannot be read pins nothing and is logged.
    pub async fn pins() -> Result<BTreeMap<String, Vec<PinningTemplate>>, ServerFnError> {
        let surreal = consume_context::<store::surreal::SurrealDb>();
        let storage = consume_context::<std::sync::Arc<dyn cloud::StorageService>>();
        let templates = store::templates::list_current(&surreal)
            .await
            .map_err(|error| ServerFnError::new(error.to_string()))?;
        let mut pins = BTreeMap::<String, Vec<PinningTemplate>>::new();
        for template in templates.into_iter().filter(|t| t.project_id.is_none()) {
            let body = match store::templates::body(&surreal, &storage, &template).await {
                Ok(body) => body,
                Err(store::templates::TemplateBodyError::MissingBody(_)) => continue,
                Err(error) => {
                    tracing::warn!(%error, template = %template.code, "clause usage: template body unreadable");
                    continue;
                }
            };
            let mut sources = rules::clauses::pinned_sources(&body);
            if sources.is_empty() {
                sources =
                    store::seed::seeded_clause_sources(&template.code, &body).unwrap_or_default();
            }
            for source in sources {
                pins.entry(source).or_default().push(PinningTemplate {
                    code: template.code.clone(),
                    title: template.title.clone(),
                });
            }
        }
        for templates in pins.values_mut() {
            templates.sort_by(|a, b| a.code.cmp(&b.code));
        }
        Ok(pins)
    }

    pub async fn catalog() -> Result<Vec<store::clauses::CatalogClause>, ServerFnError> {
        store::clauses::catalog().await.map_err(|error| {
            tracing::error!(%error, "clause library: catalog unreadable");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new("the Clause library could not be read")
        })
    }
}

/// The list page: every current Clause, narrowed by `?q=` and `?kind=`.
#[server]
pub async fn get_clause_library() -> Result<ClauseLibraryView, ServerFnError> {
    let chrome = load::chrome().await?;
    let axum::extract::Query(query) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Query<ClauseQuery>, _>()
            .await?;
    let (q, kind) = (query.q.unwrap_or_default(), query.kind.unwrap_or_default());

    let catalog = load::catalog().await?;
    let pins = load::pins().await?;
    let mut kinds: Vec<String> = catalog.iter().map(|c| c.kind.clone()).collect();
    kinds.sort();
    kinds.dedup();

    let current: Vec<_> = catalog.iter().filter(|c| c.is_current).collect();
    let rows = current
        .iter()
        .filter(|c| matches(&c.code, &c.kind, &c.body, &q, &kind))
        .map(|c| ClauseRow {
            code: c.code.clone(),
            version: c.version,
            kind: c.kind.clone(),
            inputs: c.inputs.len(),
            templates: pinning_count(&catalog, &pins, &c.code),
        })
        .collect();
    Ok(ClauseLibraryView {
        chrome,
        query: q,
        kind,
        kinds,
        total: current.len(),
        rows,
        failed: false,
    })
}

/// Distinct Templates that pin any version of `code`.
#[cfg(feature = "server")]
fn pinning_count(
    catalog: &[store::clauses::CatalogClause],
    pins: &std::collections::BTreeMap<String, Vec<PinningTemplate>>,
    code: &str,
) -> usize {
    catalog
        .iter()
        .filter(|c| c.code == code)
        .filter_map(|c| pins.get(&format!("{}@{}", c.code, c.version)))
        .flatten()
        .map(|template| template.code.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// The detail page: every version of one Clause with its pinning Templates.
#[server]
pub async fn get_clause_detail() -> Result<ClauseDetailView, ServerFnError> {
    let chrome = load::chrome().await?;
    let axum::extract::Path(code) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await?;
    let catalog = load::catalog().await?;
    let pins = load::pins().await?;
    let mut versions: Vec<VersionView> = catalog
        .into_iter()
        .filter(|c| c.code == code)
        .map(|c| VersionView {
            templates: pins
                .get(&format!("{}@{}", c.code, c.version))
                .cloned()
                .unwrap_or_default(),
            version: c.version,
            kind: c.kind,
            is_current: c.is_current,
            inputs: c
                .inputs
                .into_iter()
                .map(|(name, input)| InputView {
                    name,
                    types: input.types,
                    prompt: input.prompt,
                })
                .collect(),
            body: c.body,
        })
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(v.version));
    if versions.is_empty() {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
    }
    Ok(ClauseDetailView {
        chrome,
        code,
        versions,
        failed: false,
    })
}

fn head(chrome: &Chrome, title: &str) -> Element {
    rsx! {
        document::Title { "{chrome.firm_name} | Lawyer | {title}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{chrome.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(chrome.role),
            logo: chrome.logo.clone(),
        }
    }
}

/// The count of Templates, spelled for a screen reader and a glance.
fn templates_label(count: usize) -> String {
    match count {
        0 => "No Templates".to_string(),
        1 => "1 Template".to_string(),
        n => format!("{n} Templates"),
    }
}

/// `/app/admin/clauses`.
#[component]
pub fn ClauseLibrary() -> Element {
    let resource = use_server_future(get_clause_library)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "clause-library", p { "Failed to load the Clause library." } }
            }
        }
        None => {
            return rsx! {
                main { id: "clause-library", p { "Loading…" } }
            }
        }
    };
    let shown = view.rows.len();
    rsx! {
        {head(&view.chrome, "Clauses")}
        main { id: "clause-library", class: "nav-theme",
            header { class: "page-header",
                h1 { "Clauses" }
                p {
                    "The reusable wording Templates pin by version. Read-only: change wording by adding a version to the library."
                }
            }
            form {
                class: "clause-search",
                role: "search",
                method: "get",
                action: "{CLAUSES_PATH}",
                label { class: "nav-label", r#for: "clause-q", "Search code, kind, or wording" }
                input {
                    id: "clause-q",
                    class: "nav-input",
                    r#type: "search",
                    name: "q",
                    value: "{view.query}",
                    maxlength: "200",
                    autocomplete: "off",
                }
                label { class: "nav-label", r#for: "clause-kind", "Kind" }
                select { id: "clause-kind", class: "nav-select", name: "kind",
                    option { value: "", selected: view.kind.is_empty(), "All kinds" }
                    for kind in view.kinds.iter() {
                        option {
                            value: "{kind}",
                            selected: *kind == view.kind,
                            "{kind}"
                        }
                    }
                }
                button { class: "nav-btn nav-btn--primary", r#type: "submit", "Search" }
            }
            p { class: "clause-count", role: "status", "Showing {shown} of {view.total} Clauses" }
            div { class: "nav-table-wrap",
                table { class: "nav-table",
                    caption { class: "visually-hidden", "Current Clauses" }
                    thead {
                        tr {
                            th { scope: "col", "Code" }
                            th { scope: "col", "Version" }
                            th { scope: "col", "Kind" }
                            th { scope: "col", "Inputs" }
                            th { scope: "col", "Used by" }
                        }
                    }
                    tbody {
                        if view.rows.is_empty() {
                            tr {
                                td { colspan: "5", class: "clause-empty", "No Clauses match." }
                            }
                        }
                        for row in view.rows.iter() {
                            tr { class: "clause-row",
                                td {
                                    a { href: "{clause_href(&row.code)}", "{row.code}" }
                                }
                                td { "{row.version}" }
                                td { "{row.kind}" }
                                td { "{row.inputs}" }
                                td { "{templates_label(row.templates)}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `/app/admin/clauses/{code}`.
#[component]
pub fn ClauseDetail() -> Element {
    let resource = use_server_future(get_clause_detail)?;
    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "clause-detail", p { "Failed to load the Clause." } }
            }
        }
        None => {
            return rsx! {
                main { id: "clause-detail", p { "Loading…" } }
            }
        }
    };
    if view.versions.is_empty() {
        return rsx! {
            {head(&view.chrome, "Clause not found")}
            main { id: "clause-detail", class: "nav-theme",
                h1 { "Clause not found" }
                p { a { href: "{CLAUSES_PATH}", "← All Clauses" } }
            }
        };
    }
    rsx! {
        {head(&view.chrome, &view.code)}
        main { id: "clause-detail", class: "nav-theme",
            header { class: "page-header",
                h1 { "{view.code}" }
                p { a { href: "{CLAUSES_PATH}", "← All Clauses" } }
            }
            for version in view.versions.iter() {
                section {
                    class: "clause-version",
                    "aria-labelledby": "clause-v{version.version}",
                    h2 { id: "clause-v{version.version}",
                        "{view.code}@{version.version}"
                        if version.is_current {
                            " (current)"
                        }
                    }
                    p { "Kind: {version.kind}" }
                    h3 { "Body" }
                    pre { class: "clause-body", "{version.body}" }
                    h3 { "Inputs" }
                    if version.inputs.is_empty() {
                        p { "No inputs." }
                    } else {
                        dl { class: "clause-inputs",
                            for input in version.inputs.iter() {
                                dt { "{input.name}" }
                                dd { "{input.prompt} ({input.types.join(\", \")})" }
                            }
                        }
                    }
                    h3 { "Pinned by" }
                    if version.templates.is_empty() {
                        p { "No Template pins this version." }
                    } else {
                        ul { class: "clause-templates",
                            for template in version.templates.iter() {
                                li { "{template.title} ({template.code})" }
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

    #[test]
    fn search_reads_code_kind_and_wording_without_regard_to_case() {
        let (code, kind, body) = ("governing_law", "agreement", "Courts: {{forum}}.");
        assert!(matches(code, kind, body, "", ""));
        assert!(matches(code, kind, body, "GOVERNING", ""));
        assert!(matches(code, kind, body, "agree", ""));
        assert!(matches(code, kind, body, "courts", ""));
        assert!(!matches(code, kind, body, "arbitration", ""));
    }

    #[test]
    fn the_kind_filter_is_exact() {
        assert!(matches("a", "agreement", "", "", "agreement"));
        assert!(!matches("a", "agreement", "", "", "onboarding"));
        assert!(!matches("a", "agreement", "", "", "agree"));
    }

    #[test]
    fn the_detail_href_names_the_code() {
        assert_eq!(
            clause_href("governing_law"),
            "/app/admin/clauses/governing_law"
        );
    }

    #[test]
    fn a_template_count_reads_in_words() {
        assert_eq!(templates_label(0), "No Templates");
        assert_eq!(templates_label(1), "1 Template");
        assert_eq!(templates_label(3), "3 Templates");
    }
}
