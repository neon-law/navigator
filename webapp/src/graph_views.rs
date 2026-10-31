//! The three relationship-graph views — project, lawyer, firm — as Dioxus
//! pages over a read-only Sigma.js island.
//!
//! Each page is a thin, server-rendered shell: the heading, the depth
//! selector (anchors that re-request the page with a new `?depth=`), a
//! static legend, and one mount `<div>` carrying data attributes — the AGE
//! endpoint URL (with the depth already baked in) and, per vertex kind, a
//! click-through href template the viewer's tier may actually open. The
//! first-party island (`/public/js/graph-view.js`, vendored Sigma.js +
//! Graphology) fetches the endpoint JSON in the browser and draws it, so the
//! server's depth cap and tenancy checks stay the only boundary: the island
//! never sees a URL the handler did not already gate.
//!
//! The server functions re-check the tier with [`crate::admin_listing::require_lawyer`]
//! — the same defense-in-depth shape as the other gated pages — and the
//! project page resolves the matter code in the path to the internal id the
//! `GET /app/api/graph/project/{id}` endpoint is addressed by.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::people::ViewerRole;

/// The first-party island that mounts Sigma.js onto the graph view mounts.
pub const GRAPH_VIEW_SCRIPT_HREF: &str = "/public/js/graph-view.js";

/// The server's hop ceiling — the same cap `store::postgres_graph_views`
/// enforces and the REST door rejects above. The depth selector never offers
/// a value past it, and the loader clamps to it, so the island's fetches stay
/// inside what `graph_depth` admits.
pub const MAX_GRAPH_DEPTH: u8 = 3;

/// The depth the selector offers first, matching the endpoint's own default.
const DEFAULT_DEPTH: u8 = 2;

/// One rendered graph page: everything the shell and the island both need,
/// in a wasm-safe shape.
#[derive(Serialize, Deserialize, Clone, PartialEq)]
pub struct GraphPageView {
    /// The resolved brand's tokens stylesheet href, so the page wears
    /// its own palette rather than the firm's on a non-default host.
    #[serde(default)]
    pub tokens_href: String,
    /// The deploy's firm name, for the document title.
    #[serde(default)]
    pub firm_name: String,
    /// What the reader came to see — the matter, person, or firm name. An
    /// empty heading is the not-found case: the seed record does not exist
    /// (or the caller may not even know it does), so the page renders the
    /// non-disclosing not-found body.
    #[serde(default)]
    pub heading: String,
    /// Where the reader came from, as the back link's text.
    pub back_text: String,
    /// Where the reader came from, as the back link's href.
    pub back_href: String,
    /// The `GET /app/api/graph/{view}/{id}?depth=` URL the island fetches,
    /// with the depth already resolved server-side.
    pub endpoint: String,
    /// The active depth, clamped into `1..=[MAX_GRAPH_DEPTH]`.
    pub depth: u8,
    /// Click-through href templates, one per vertex kind that has a record
    /// page this viewer's tier may open. `{id}` is the placeholder the
    /// island replaces with the node id. `None` means "no page to open" —
    /// for a tier that cannot reach the record page, the node is simply
    /// not clickable-through.
    #[serde(default)]
    pub href_person: Option<String>,
    #[serde(default)]
    pub href_firm: Option<String>,
    #[serde(default)]
    pub href_entity: Option<String>,
}

/// The graph page's `?depth=` query. Lenient like the matter surface's
/// `?tab=`: a value the strip never advertised falls back to the default
/// rather than refusing the page.
#[derive(Deserialize, Default)]
pub struct GraphDepthQuery {
    #[serde(default)]
    pub depth: Option<String>,
}

/// Parse the `?depth=` value: clamped into `1..=[MAX_GRAPH_DEPTH]`, with
/// anything absent, non-numeric, or out of range falling back to
/// [`DEFAULT_DEPTH`] rather than a 400 — the island can only ever fetch what
/// this page rendered, so a bad depth is a stale tab, not an attack.
#[must_use]
pub fn parse_depth(raw: Option<&str>) -> u8 {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return DEFAULT_DEPTH;
    };
    raw.parse::<u8>()
        .ok()
        .filter(|depth| (1..=MAX_GRAPH_DEPTH).contains(depth))
        .unwrap_or(DEFAULT_DEPTH)
}

/// The click-through template for a vertex kind, given the viewer's tier.
/// Person and Firm record pages are the admin console's; the Entity edit page
/// is the lawyer-tier directory's. A template carries `{id}` for the island
/// to replace with the node id.
#[must_use]
pub fn href_templates(role: ViewerRole) -> (Option<String>, Option<String>, Option<String>) {
    const PERSON: &str = "/app/admin/people/{id}";
    const FIRM: &str = "/app/admin/firms/{id}";
    const ENTITY: &str = "/app/admin/entities/{id}/edit";
    if role.is_admin_tier() {
        (
            Some(PERSON.to_string()),
            Some(FIRM.to_string()),
            Some(ENTITY.to_string()),
        )
    } else if role.is_lawyer_tier() {
        (None, None, Some(ENTITY.to_string()))
    } else {
        (None, None, None)
    }
}

/// The pieces every graph page's loader assembles from the request context.
/// Server-only — the wasm client renders the shell from the passed-in view.
#[cfg(feature = "server")]
struct PageFrame {
    role: ViewerRole,
    tokens_href: String,
    firm_name: String,
}

/// Read the request context the shell needs — the viewer's tier and the
/// resolved brand chrome. Server-only.
#[cfg(feature = "server")]
async fn page_frame() -> Result<PageFrame, ServerFnError> {
    let role = crate::admin_listing::require_lawyer().await?;
    Ok(PageFrame {
        role,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        firm_name: crate::app_chrome::firm_name_from_context().await,
    })
}

/// The not-found view: an empty heading tells the component to render the
/// non-disclosing body. Server-only.
#[cfg(feature = "server")]
fn not_found_view() -> GraphPageView {
    GraphPageView {
        tokens_href: String::new(),
        firm_name: String::new(),
        heading: String::new(),
        back_text: String::new(),
        back_href: String::new(),
        endpoint: String::new(),
        depth: DEFAULT_DEPTH,
        href_person: None,
        href_firm: None,
        href_entity: None,
    }
}

/// Commit the `404` for a seed record that does not exist, so the status
/// matches the page rather than a `200` carrying a not-found body.
#[cfg(feature = "server")]
fn commit_not_found() {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::NOT_FOUND,
        None,
    );
}

/// `GET /app/projects/{project_code}/graph` — the matter's conflict graph.
/// Resolves the matter code in the path to the internal id the graph
/// endpoint is addressed by; a matter the store does not know gets the same
/// non-disclosing not-found body the matter page gives.
#[server]
pub async fn get_project_graph_page() -> Result<GraphPageView, ServerFnError> {
    let frame = page_frame().await?;
    let Ok(axum::extract::Path(code)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>().await
    else {
        commit_not_found();
        return Ok(not_found_view());
    };
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<GraphDepthQuery>,
        _,
    >()
    .await
    .unwrap_or_default();
    let depth = parse_depth(query.depth.as_deref());

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .map_err(|e| ServerFnError::new(e.to_string()))?
    else {
        commit_not_found();
        return Ok(not_found_view());
    };

    // The endpoint scopes the project graph with
    // `store::access::can_see_project` — the participation ledger — and this
    // shell discloses the matter's name, so the page applies the same gate
    // before rendering it. A caller with no view of the matter gets the same
    // non-disclosing not-found body the matter page gives, and the failure
    // reads identically for "no such matter" and "not your matter".
    let person_id = dioxus_fullstack_core::FullstackContext::extract::<
        axum::Extension<crate::portal_project_list::PersonId>,
        _,
    >()
    .await
    .ok()
    .and_then(|axum::Extension(pid)| pid.0)
    .and_then(|raw| raw.parse::<uuid::Uuid>().ok());
    let store_role = match frame.role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };
    let in_scope = store::access::can_see_project(&surreal, person_id, store_role, project.id)
        .await
        .unwrap_or(false);
    if !in_scope {
        commit_not_found();
        return Ok(not_found_view());
    }

    let (href_person, href_firm, href_entity) = href_templates(frame.role);
    Ok(GraphPageView {
        heading: project.name,
        back_text: "← Back to the matter".to_string(),
        back_href: format!("/app/projects/{code}"),
        endpoint: format!("/app/api/graph/project/{}?depth={depth}", project.id),
        depth,
        tokens_href: frame.tokens_href,
        firm_name: frame.firm_name,
        href_person,
        href_firm,
        href_entity,
    })
}

/// `GET /app/people/{id}/graph` — one person's working neighborhood. The page
/// renders the shell; the endpoint itself decides who may read which person's
/// graph (the person themself or the firm directory) and answers the
/// non-disclosing `404`, which the island reports as "not available for your
/// view".
#[server]
pub async fn get_lawyer_graph_page() -> Result<GraphPageView, ServerFnError> {
    let frame = page_frame().await?;
    let Ok(axum::extract::Path(id)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await
    else {
        commit_not_found();
        return Ok(not_found_view());
    };
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<GraphDepthQuery>,
        _,
    >()
    .await
    .unwrap_or_default();
    let depth = parse_depth(query.depth.as_deref());

    let (href_person, href_firm, href_entity) = href_templates(frame.role);
    Ok(GraphPageView {
        heading: "Working neighborhood".to_string(),
        back_text: "← Back to the directory".to_string(),
        back_href: "/app/admin/people".to_string(),
        endpoint: format!("/app/api/graph/lawyer/{id}?depth={depth}"),
        depth,
        tokens_href: frame.tokens_href,
        firm_name: frame.firm_name,
        href_person,
        href_firm,
        href_entity,
    })
}

/// `GET /app/admin/firms/{id}/graph` — the firm-to-firm strategic view.
/// The endpoint enforces `FirmCapability::ViewDirectory` on the seed firm;
/// the island reports the non-disclosing `404` the same way.
#[server]
pub async fn get_firm_graph_page() -> Result<GraphPageView, ServerFnError> {
    let frame = page_frame().await?;
    let Ok(axum::extract::Path(id)) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<uuid::Uuid>, _>()
            .await
    else {
        commit_not_found();
        return Ok(not_found_view());
    };
    let (href_person, href_firm, href_entity) = href_templates(frame.role);
    Ok(GraphPageView {
        heading: "Firm graph".to_string(),
        back_text: "← Back to the workbench".to_string(),
        back_href: "/app/lawyer".to_string(),
        endpoint: format!("/app/api/graph/firm/{id}"),
        depth: MAX_GRAPH_DEPTH,
        tokens_href: frame.tokens_href,
        firm_name: frame.firm_name,
        href_person,
        href_firm,
        href_entity,
    })
}

/// The project graph view, server-side rendered. The island below does the
/// drawing; this shell only decides what the island may fetch.
#[component]
pub fn ProjectGraphPage() -> Element {
    graph_page_body(use_server_future(get_project_graph_page)?)
}

/// The lawyer graph view, server-side rendered.
#[component]
pub fn LawyerGraphPage() -> Element {
    graph_page_body(use_server_future(get_lawyer_graph_page)?)
}

/// The firm graph view, server-side rendered. The endpoint takes no
/// meaningful depth (the aggregation is a fixed three-hop read), so the page
/// renders no depth selector.
#[component]
pub fn FirmGraphPage() -> Element {
    graph_page_body(use_server_future(get_firm_graph_page)?)
}

/// The shared loading/error/not-found switch: every graph page reads its
/// view through one resource and falls into the same bodies.
fn graph_page_body(resource: Resource<Result<GraphPageView, ServerFnError>>) -> Element {
    let view = match &*resource.read() {
        Some(Ok(view)) if !view.heading.is_empty() => view.clone(),
        Some(Ok(_)) => {
            return not_found_body();
        }
        Some(Err(_)) => {
            return rsx! {
                main { id: "graph-view", class: "nav-theme", p { "Failed to load the graph page." } }
            }
        }
        None => {
            return rsx! {
                main { id: "graph-view", class: "nav-theme", p { "Loading…" } }
            }
        }
    };
    rsx! {
        GraphPageShell { view }
    }
}

/// The non-disclosing not-found body — the same words the matter page uses
/// for a seed the caller may not see.
fn not_found_body() -> Element {
    rsx! {
        main { id: "graph-view", class: "nav-theme", p { "That record was not found." } }
    }
}

/// The graph page shell: chrome, depth selector, legend, and the one mount
/// `<div>` the island fills. Without JavaScript the status line says the
/// graph needs it — the same graceful fallback the PDF viewer applies.
#[component]
pub fn GraphPageShell(view: GraphPageView) -> Element {
    let GraphPageView {
        tokens_href,
        firm_name,
        heading,
        back_text,
        back_href,
        endpoint,
        depth,
        href_person,
        href_firm,
        href_entity,
    } = view;

    rsx! {
        document::Title { "{firm_name} | Graph | {heading}" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{tokens_href}" }
        nav { class: "lawyer-nav",
            a { class: "nav-link", href: "/app/projects", "Projects" }
            a { class: "nav-link", href: "/auth/logout", "Sign out" }
            crate::components::AppProfileMenu {
                destinations: vec![
                    crate::components::AppNavLink::new("Projects", "/app/projects"),
                    crate::components::AppNavLink::new("Sign out", "/auth/logout"),
                ],
            }
        }
        main { id: "graph-view", class: "nav-theme",
            header { class: "page-header",
                h1 { "Graph: {heading}" }
                p { a { class: "nav-link", href: "{back_href}", "{back_text}" } }
                DepthSelector { depth }
                GraphLegend {}
            }
            div {
                class: "graph-view",
                "data-endpoint": "{endpoint}",
                "data-href-person": href_person.unwrap_or_default(),
                "data-href-firm": href_firm.unwrap_or_default(),
                "data-href-entity": href_entity.unwrap_or_default(),
                role: "region",
                "aria_label": "Relationship graph",
                p { class: "muted graph-view__status", "Loading graph…" }
            }
            document::Script { src: GRAPH_VIEW_SCRIPT_HREF, defer: true }
        }
    }
}

/// The depth selector: anchors that re-request the page with a new
/// `?depth=`, the active one marked `aria-current`. The server re-resolves
/// and clamps the value, so the island can only ever fetch what the server
/// rendered.
#[component]
pub fn DepthSelector(depth: u8) -> Element {
    if depth == MAX_GRAPH_DEPTH {
        // At the ceiling there is nothing to raise to.
        return rsx! {};
    }
    rsx! {
        nav { class: "graph-depth", "aria-label": "Graph depth",
            "Depth: "
            for candidate in 1..=MAX_GRAPH_DEPTH {
                if candidate == depth {
                    a {
                        class: "nav-link graph-depth__option graph-depth__option--active",
                        href: "?depth={candidate}",
                        "aria-current": "true",
                        "{candidate}"
                    }
                } else {
                    a { class: "nav-link graph-depth__option", href: "?depth={candidate}", "{candidate}" }
                }
            }
        }
    }
}

/// The static legend: what each vertex color means, and how the edges read.
/// The colors are the island's `NODE_COLORS` — kept in step by comment in
/// both places, since the legend is server-rendered and the drawing is not.
#[component]
pub fn GraphLegend() -> Element {
    rsx! {
        ul { class: "graph-legend", "aria-label": "Graph legend",
            li { span { class: "graph-legend__chip graph-legend__chip--person", "" } "Person" }
            li { span { class: "graph-legend__chip graph-legend__chip--entity", "" } "Entity" }
            li { span { class: "graph-legend__chip graph-legend__chip--project", "" } "Project" }
            li { span { class: "graph-legend__chip graph-legend__chip--firm", "" } "Firm" }
            li { class: "graph-legend__note",
                "Red edges are adverse; an edge's weight is its confidence. Hover for detail, click a node to focus it."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_defaults_when_absent_or_blank() {
        assert_eq!(parse_depth(None), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some("")), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some("  ")), DEFAULT_DEPTH);
    }

    #[test]
    fn depth_parses_and_clamps_into_the_server_cap() {
        assert_eq!(parse_depth(Some("1")), 1);
        assert_eq!(parse_depth(Some("2")), 2);
        assert_eq!(parse_depth(Some("3")), MAX_GRAPH_DEPTH);
        // Above the cap falls back rather than escaping the server's ceiling.
        assert_eq!(parse_depth(Some("4")), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some("255")), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some("0")), DEFAULT_DEPTH);
    }

    #[test]
    fn depth_is_lenient_about_noise() {
        assert_eq!(parse_depth(Some("two")), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some("-1")), DEFAULT_DEPTH);
        assert_eq!(parse_depth(Some(" 2 ")), 2);
    }

    #[test]
    fn click_through_templates_follow_the_tier() {
        let (person, firm, entity) = href_templates(ViewerRole::Owner);
        assert_eq!(person.as_deref(), Some("/app/admin/people/{id}"));
        assert_eq!(firm.as_deref(), Some("/app/admin/firms/{id}"));
        assert_eq!(entity.as_deref(), Some("/app/admin/entities/{id}/edit"));

        let (person, firm, entity) = href_templates(ViewerRole::Lawyer);
        assert_eq!(person, None, "the person record page is admin-tier");
        assert_eq!(firm, None, "the firm record page is admin-tier");
        assert_eq!(entity.as_deref(), Some("/app/admin/entities/{id}/edit"));

        let (person, firm, entity) = href_templates(ViewerRole::Client);
        assert_eq!(person, None);
        assert_eq!(firm, None);
        assert_eq!(entity, None);
    }

    #[test]
    fn the_page_carries_the_island() {
        assert_eq!(GRAPH_VIEW_SCRIPT_HREF, "/public/js/graph-view.js");
    }

    // The page's ceiling restates the store's — when the store's cap moves,
    // this fails until the selector moves with it. Server-feature-gated
    // because the store crate itself is.
    #[cfg(feature = "server")]
    #[test]
    fn the_depth_cap_agrees_with_the_store() {
        assert_eq!(
            MAX_GRAPH_DEPTH,
            store::postgres_graph_views::MAX_GRAPH_DEPTH
        );
    }
}
