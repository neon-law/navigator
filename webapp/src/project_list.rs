//! The firm-side projects list at `/app/projects`: every matter the reader
//! may reach, as code, name, Brand, and status.
//!
//! The list carries only what a reader scans for. Entity, dates, and the last
//! commit live on the matter's show page at `/app/projects/{code}`, which is
//! where every row links.
//!
//! A Lawyer sees the matters they participate in firm-side. An Admin also
//! sees every matter in a Firm they administer, including one nobody is on
//! yet, and an Owner sees every matter (see [`accessible_projects`]).
//!
//! Listing a matter is not opening it. Every firm tier needs a firm-side
//! participation row to open the matter surface at `/app/projects/{code}`.
//!
//! Status is the matter-lifecycle pill (`store::projects::matter_lifecycle`):
//! `open`, `pitch`, `active`, or `closed`, with an archived matter reading `closed`.
//! The `?status=` filter is a multi-select over those three, comma-separated;
//! with no `?status=` the list shows `pitch` and `active`. The `?brand=`
//! filter is a multi-select over the brand keys among the viewer's matters,
//! comma-separated; with no `?brand=` every brand shows.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Column, DataTable, SortState};
use crate::people::ViewerRole;
#[cfg(feature = "server")]
use crate::portal_project_list::PersonId;

/// Every status the filter offers, in lifecycle order.
pub const PROJECT_STATUSES: [&str; 4] = ["open", "pitch", "active", "closed"];

/// The statuses selected when the request names none.
pub const DEFAULT_PROJECT_STATUSES: [&str; 3] = ["open", "pitch", "active"];

/// The `?status=` value that selects no status. Any value the filter does
/// not offer selects nothing, so this one only needs to be stable.
const NO_STATUS: &str = "none";

/// One Brand the filter row offers: its registry key (the `?brand=` value)
/// and its display name.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BrandFacet {
    pub key: String,
    pub name: String,
}

/// One active label offered by a visible Project.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LabelFacet {
    pub category: String,
    pub name: String,
    pub id: String,
}

/// One matter row, in a wasm-safe shape (plain fields — no `store` types cross
/// to the client build).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub code: String,
    pub name: String,
    /// The Brand's display name, or the raw key when no `brand` row matches.
    pub brand_name: String,
    #[serde(default)]
    pub labels: Vec<LabelFacet>,
    /// `store::projects::MatterLifecycle::class()` — the pill's CSS class.
    pub lifecycle_class: String,
    /// `store::projects::MatterLifecycle::label()` — the status the filter
    /// matches and the pill's text, so the state never rests on colour alone.
    pub lifecycle_label: String,
    /// `store::projects::MatterLifecycle::title()` — the hover and assistive
    /// text stating what the pill did and did not verify.
    pub lifecycle_title: String,
}

/// The rendered projects list.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ProjectListView {
    pub rows: Vec<ProjectRow>,
    pub sort: String,
    pub role: ViewerRole,
    /// The selected statuses, in [`PROJECT_STATUSES`] order.
    pub statuses: Vec<String>,
    /// The brand keys selected by `?brand=`, lowercased; empty when every
    /// brand shows.
    #[serde(default)]
    pub brands: Vec<String>,
    /// The brand facets the filter row offers: every brand key among the
    /// viewer's matters, with its resolved display name.
    #[serde(default)]
    pub brand_facets: Vec<BrandFacet>,
    #[serde(default)]
    pub label_facets: Vec<LabelFacet>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// The deploy's brand mark for the navbar.
    #[serde(default)]
    pub logo: Option<crate::components::AppLogo>,
    /// The resolved brand's tokens stylesheet href.
    #[serde(default)]
    pub tokens_href: String,
    /// The `?error=` flash a refused matter delete redirects back with.
    #[serde(default)]
    pub error: Option<String>,
    /// The deploy's firm name, for the document title.
    #[serde(default)]
    pub firm_name: String,
}

/// The projects list query.
#[derive(Deserialize, Default)]
pub struct ProjectListQuery {
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// The statuses a `?status=` value selects, in [`PROJECT_STATUSES`] order.
#[must_use]
pub fn selected_statuses(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return DEFAULT_PROJECT_STATUSES.map(str::to_string).to_vec();
    };
    PROJECT_STATUSES
        .iter()
        .filter(|status| {
            raw.split(',')
                .any(|value| value.trim().eq_ignore_ascii_case(status))
        })
        .map(|status| (*status).to_string())
        .collect()
}

/// `selected` with `status` added or removed, in [`PROJECT_STATUSES`] order.
fn toggle_status(selected: &[String], status: &str) -> Vec<String> {
    PROJECT_STATUSES
        .iter()
        .filter(|candidate| (**candidate == status) != selected.iter().any(|s| s == *candidate))
        .map(|candidate| (*candidate).to_string())
        .collect()
}

/// The query pairs that carry `selected`: none for the default selection.
fn status_query(selected: &[String]) -> Vec<(String, String)> {
    if selected
        .iter()
        .map(String::as_str)
        .eq(DEFAULT_PROJECT_STATUSES)
    {
        Vec::new()
    } else if selected.is_empty() {
        vec![("status".to_string(), NO_STATUS.to_string())]
    } else {
        vec![("status".to_string(), selected.join(","))]
    }
}

/// Parse the selected keys of a comma-separated filter.
#[must_use]
pub fn selected_filter_keys(raw: Option<&str>) -> Vec<String> {
    let mut selected: Vec<String> = Vec::new();
    for value in raw.unwrap_or_default().split(',') {
        let key = value.trim().to_lowercase();
        if !key.is_empty() && !selected.contains(&key) {
            selected.push(key);
        }
    }
    selected
}

/// `selected` with `key` added or removed.
fn toggle_key(selected: &[String], key: &str) -> Vec<String> {
    let mut next = selected.to_vec();
    match next.iter().position(|candidate| candidate == key) {
        Some(position) => {
            next.remove(position);
        }
        None => next.push(key.to_lowercase()),
    }
    next
}

/// The query pairs that carry the brand filter: none while every brand shows.
fn brand_query(selected: &[String]) -> Vec<(String, String)> {
    if selected.is_empty() {
        Vec::new()
    } else {
        vec![("brand".to_string(), selected.join(","))]
    }
}

/// Filter query pairs preserved by sort links and filter toggles.
fn filters_query(
    statuses: &[String],
    brands: &[String],
    labels: &[String],
) -> Vec<(String, String)> {
    let mut pairs = status_query(statuses);
    pairs.extend(brand_query(brands));
    if !labels.is_empty() {
        pairs.push(("label".to_string(), labels.join(",")));
    }
    pairs
}

/// The list's href for `query`, keeping the current sort.
fn list_href(sort: &str, query: &[(String, String)]) -> String {
    if query.is_empty() && sort.is_empty() {
        "/app/projects".to_string()
    } else {
        crate::components::data_table::sort_href("/app/projects", sort, query)
    }
}

/// Commit a `500` before failing the loader, so an unavailable directory is a
/// server error rather than a `200` with an error body.
#[cfg(feature = "server")]
fn loader_error(e: impl std::fmt::Display) -> ServerFnError {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        None,
    );
    ServerFnError::new(e.to_string())
}

/// Parse a JSON:API `sort` value into `(key, descending)` pairs.
#[cfg(feature = "server")]
fn parse_sort(raw: &str) -> Vec<(String, bool)> {
    raw.split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && *segment != "-")
        .map(|segment| match segment.strip_prefix('-') {
            Some(key) => (key.to_string(), true),
            None => (segment.to_string(), false),
        })
        .collect()
}

/// One composite comparator: the first requested `?sort=` field is primary
/// and later fields only break ties (the JSON:API `SortSpec` contract).
#[cfg(feature = "server")]
fn sort_rows(rows: &mut [ProjectRow], parsed: &[(String, bool)]) {
    rows.sort_by(|a, b| {
        parsed
            .iter()
            .fold(std::cmp::Ordering::Equal, |acc, (key, descending)| {
                acc.then_with(|| {
                    let ordering = match key.as_str() {
                        "code" => a.code.cmp(&b.code),
                        "name" => a.name.cmp(&b.name),
                        "status" => a.lifecycle_label.cmp(&b.lifecycle_label),
                        "brand" => a.brand_name.cmp(&b.brand_name),
                        _ => std::cmp::Ordering::Equal,
                    };
                    if *descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                })
            })
    });
}

/// Build one row. An archived matter is as finished as a closed one, so it
/// reads `closed`.
#[cfg(feature = "server")]
fn project_row(
    brand_name: String,
    m: store::projects::Project,
    has_engagement: bool,
    has_closing: bool,
    labels: Vec<LabelFacet>,
) -> ProjectRow {
    let status = if m.status.eq_ignore_ascii_case("archived") {
        "closed"
    } else {
        m.status.as_str()
    };
    let (missing_onboarding, missing_offboarding_letter) =
        store::projects::matter_flags(has_engagement, status, has_closing);
    let lifecycle = store::projects::project_lifecycle(
        &m.code,
        status,
        missing_onboarding,
        missing_offboarding_letter,
    );
    ProjectRow {
        brand_name,
        labels,
        code: m.code,
        name: m.name,
        lifecycle_class: lifecycle.class().to_string(),
        lifecycle_label: lifecycle.label().to_string(),
        lifecycle_title: lifecycle.title().to_string(),
    }
}

/// The matters the reader may reach from this list. An Owner holds every Firm
/// capability, so every matter. An Admin also assigns people to matters
/// nobody is on yet, so every matter in a Firm whose membership they administer, and every
/// matter no Firm owns. Everyone else sees the matters they participate in
/// firm-side.
#[cfg(feature = "server")]
async fn accessible_projects(
    application_store: &store::application_store::ApplicationStore,
    person_id: Option<uuid::Uuid>,
    role: store::persons::Role,
) -> Result<Vec<store::projects::Project>, ServerFnError> {
    use store::persons::Role;

    if role == Role::Owner {
        return store::projects::all(application_store)
            .await
            .map_err(loader_error);
    }
    let mut matters = store::access::visible_projects_as_lawyer(application_store, person_id, role)
        .await
        .map_err(loader_error)?;
    if role != Role::Admin {
        return Ok(matters);
    }
    let mut administered = std::collections::HashMap::new();
    for project in store::projects::all(application_store)
        .await
        .map_err(loader_error)?
    {
        if matters.iter().any(|m| m.id == project.id) {
            continue;
        }
        let allowed = match project.firm_id {
            Some(firm_id) => {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    administered.entry(firm_id)
                {
                    let allowed = store::projects::can_administer_project(
                        application_store,
                        person_id,
                        role,
                        &project,
                    )
                    .await
                    .map_err(loader_error)?;
                    entry.insert(allowed);
                }
                administered[&firm_id]
            }
            None => store::projects::can_administer_project(
                application_store,
                person_id,
                role,
                &project,
            )
            .await
            .map_err(loader_error)?,
        };
        if allowed {
            matters.push(project);
        }
    }
    Ok(matters)
}

/// One facet per distinct brand key among the viewer's matters, labelled with
/// the brand's resolved display name (the raw key when the registry has none).
#[cfg(feature = "server")]
fn brand_facets_from(
    matters: &[store::projects::Project],
    brands: &[store::brands::Brand],
) -> Vec<BrandFacet> {
    let mut facets: Vec<BrandFacet> = Vec::new();
    for matter in matters {
        if facets.iter().any(|facet| facet.key == matter.brand) {
            continue;
        }
        let name = brands
            .iter()
            .find(|b| b.key == matter.brand)
            .map_or(matter.brand.as_str(), |b| b.name.as_str())
            .to_string();
        facets.push(BrandFacet {
            key: matter.brand.clone(),
            name,
        });
    }
    facets
}

/// Fetch the projects list for the current request. A non-firm caller gets a
/// `404`: the directory is hidden from them, not merely refused.
#[server]
pub async fn get_project_list() -> Result<ProjectListView, ServerFnError> {
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    if !role.is_lawyer_tier() {
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::NOT_FOUND,
            None,
        );
        return Err(ServerFnError::new("not found"));
    }
    let PersonId(person_id) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<PersonId>, _>()
            .await
            .map(|axum::Extension(id)| id)
            .unwrap_or_default();
    let person_id = person_id.and_then(|raw| raw.parse::<uuid::Uuid>().ok());
    let axum::extract::Query(query) = dioxus_fullstack_core::FullstackContext::extract::<
        axum::extract::Query<ProjectListQuery>,
        _,
    >()
    .await?;
    let sort = query.sort.unwrap_or_default();
    let statuses = selected_statuses(query.status.as_deref());
    let brand_keys = selected_filter_keys(query.brand.as_deref());
    let label_ids = selected_filter_keys(query.label.as_deref());
    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };

    let application_store = consume_context::<store::application_store::ApplicationStore>();
    let matters = accessible_projects(&application_store, person_id, store_role).await?;
    let mut brands = store::brands::system_wide(&application_store)
        .await
        .map_err(loader_error)?;
    brands.extend(
        store::brands::all_firm_scoped(&application_store)
            .await
            .map_err(loader_error)?,
    );
    // A failed lifecycle lookup propagates rather than badging every matter
    // as a pitch.
    let (_, has_closing) = store::projects::matter_lifecycle_sets(&application_store, &matters)
        .await
        .map_err(loader_error)?;

    let has_engagement = store::projects::onboarding_official_sets(&application_store, &matters)
        .await
        .map_err(loader_error)?;

    // Brand facets come from the viewer's matters before the brand filter is
    // applied, so a chip never offers a value that would select nothing here.
    let brand_facets = brand_facets_from(&matters, &brands);
    let (mut project_labels, label_facets) = active_label_facets(&matters).await?;
    let selected_labels: Vec<_> = label_facets
        .iter()
        .filter(|label| label_ids.contains(&label.id))
        .collect();

    let mut rows: Vec<ProjectRow> = matters
        .into_iter()
        // The brand filter matches the raw registry key, before the row's
        // brand-name resolution.
        .filter(|m| {
            brand_keys.is_empty()
                || brand_keys
                    .iter()
                    .any(|key| m.brand.eq_ignore_ascii_case(key))
        })
        .map(|m| {
            let brand_name = brands
                .iter()
                .find(|b| b.key == m.brand)
                .map_or(m.brand.as_str(), |b| b.name.as_str())
                .to_string();
            let has_eng = has_engagement.contains(&m.id);
            let has_close = has_closing.contains(&m.id);
            let labels = project_labels.remove(&m.id).unwrap_or_default();
            project_row(brand_name, m, has_eng, has_close, labels)
        })
        .filter(|row| {
            statuses.contains(&row.lifecycle_label) && labels_match(&row.labels, &selected_labels)
        })
        .collect();
    sort_rows(&mut rows, &parse_sort(&sort));
    let labels = selected_labels
        .into_iter()
        .map(|label| label.id.clone())
        .collect();

    Ok(ProjectListView {
        firm_name: crate::app_chrome::firm_name_from_context().await,
        rows,
        sort,
        role,
        statuses,
        brands: brand_keys,
        brand_facets,
        label_facets,
        labels,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        error: query.error.filter(|message| !message.is_empty()),
    })
}

#[cfg(feature = "server")]
async fn active_label_facets(
    matters: &[store::projects::Project],
) -> Result<
    (
        std::collections::HashMap<uuid::Uuid, Vec<LabelFacet>>,
        Vec<LabelFacet>,
    ),
    ServerFnError,
> {
    let project_ids: Vec<_> = matters.iter().map(|matter| matter.id).collect();
    let pg = store::postgres::PostgresDb::application()
        .await
        .map_err(loader_error)?;
    let labels = store::project_labels::active_for_projects(&pg, &project_ids)
        .await
        .map_err(loader_error)?;
    let project_labels: std::collections::HashMap<_, Vec<_>> = labels
        .into_iter()
        .map(|(id, labels)| (id, labels.iter().map(label_facet).collect()))
        .collect();
    let facets = project_labels
        .values()
        .flatten()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok((project_labels, facets))
}

#[cfg(feature = "server")]
fn label_facet(label: &store::project_labels::Label) -> LabelFacet {
    LabelFacet {
        id: label.id.to_string(),
        name: label.name.clone(),
        category: label.category.clone(),
    }
}

#[cfg(any(feature = "server", test))]
fn labels_match(labels: &[LabelFacet], selected: &[&LabelFacet]) -> bool {
    selected.iter().all(|selection| {
        selected.iter().any(|candidate| {
            candidate.category == selection.category
                && labels.iter().any(|label| label.id == candidate.id)
        })
    })
}

/// One toggle chip in a labelled filter row.
struct FilterChip {
    key: String,
    label: String,
    selected: bool,
    href: String,
}

/// The Status filter row: one chip per lifecycle status, the list's standing
/// facets.
fn status_chip_row(view: &ProjectListView) -> Vec<FilterChip> {
    PROJECT_STATUSES
        .iter()
        .map(|status| {
            let selected = view.statuses.iter().any(|s| s == status);
            FilterChip {
                key: (*status).to_string(),
                label: (*status).to_string(),
                selected,
                href: list_href(
                    &view.sort,
                    &filters_query(
                        &toggle_status(&view.statuses, status),
                        &view.brands,
                        &view.labels,
                    ),
                ),
            }
        })
        .collect()
}

/// The Brand filter row: one chip per brand among the viewer's matters,
/// labelled with the brand's resolved display name.
fn brand_chip_row(view: &ProjectListView) -> Vec<FilterChip> {
    view.brand_facets
        .iter()
        .map(|facet| {
            let selected = view
                .brands
                .iter()
                .any(|key| facet.key.eq_ignore_ascii_case(key));
            FilterChip {
                key: facet.key.clone(),
                label: facet.name.clone(),
                selected,
                href: list_href(
                    &view.sort,
                    &filters_query(
                        &view.statuses,
                        &toggle_key(&view.brands, &facet.key),
                        &view.labels,
                    ),
                ),
            }
        })
        .collect()
}

fn label_groups(view: &ProjectListView) -> Vec<(&str, Vec<FilterChip>)> {
    view.label_facets
        .chunk_by(|left, right| left.category == right.category)
        .map(|group| {
            let chips = group
                .iter()
                .map(|label| FilterChip {
                    key: label.id.clone(),
                    label: label.name.clone(),
                    selected: view.labels.contains(&label.id),
                    href: list_href(
                        &view.sort,
                        &filters_query(
                            &view.statuses,
                            &view.brands,
                            &toggle_key(&view.labels, &label.id),
                        ),
                    ),
                })
                .collect();
            (group[0].category.as_str(), chips)
        })
        .collect()
}

/// A labelled multi-select filter row: every chip is a link toggling its one
/// value in or out of the query string, so selection needs no client JS.
fn filter_row(label: &str, chips: &[FilterChip]) -> Element {
    rsx! {
        nav { class: "nav-tabs", role: "group", "aria-label": "{label}",
            for chip in chips.iter() {
                a { key: "{chip.key}",
                    class: if chip.selected { "nav-tab is-active" } else { "nav-tab" },
                    href: "{chip.href}",
                    "aria-current": if chip.selected { "true" } else { "false" },
                    "{chip.label}"
                }
            }
        }
    }
}

/// The empty list's paragraph. A filtered-empty view recovers through a
/// Clear-filters link; a firm with no matters points at Add project.
fn empty_message(sort: &str, filters_active: bool) -> Element {
    if filters_active {
        rsx! {
            p { class: "projects-empty",
                "No projects match these filters. "
                a { href: "{list_href(sort, &[])}", "Clear filters" }
            }
        }
    } else {
        rsx! {
            p { class: "projects-empty",
                "No projects match. "
                a { href: "/app/projects/new", "Add a project." }
            }
        }
    }
}

/// The firm-side projects list: server-rendered rows, sort headers and status
/// toggles as real anchors, each row linking to the matter's show page.
#[component]
pub fn LawyerProjects() -> Element {
    let resource = use_server_future(get_project_list)?;

    let view = match &*resource.read() {
        Some(Ok(view)) => view.clone(),
        Some(Err(_)) => {
            return rsx! {
                main { id: "projects", p { "Failed to load projects." } }
            }
        }
        None => {
            return rsx! {
                main { id: "projects", p { "Loading…" } }
            }
        }
    };

    let sort = SortState::parse(Some(&view.sort));
    let extra_query = filters_query(&view.statuses, &view.brands, &view.labels);
    let status_chips = status_chip_row(&view);
    let brand_chips = brand_chip_row(&view);
    let filters_active = !extra_query.is_empty();
    let mut columns = vec![
        Column::sortable("code", "Code"),
        Column::sortable("name", "Name"),
        Column::sortable("brand", "Brand"),
        Column::sortable("status", "Status"),
    ];
    let show_labels = !view.label_facets.is_empty();
    if show_labels {
        columns.push(Column::fixed("labels", "Labels"));
    }
    let label_groups = label_groups(&view);
    let error = view.error.clone();
    let is_empty = view.rows.is_empty();

    rsx! {
        document::Title { "{view.firm_name} | Lawyer | Projects" }
        document::Stylesheet { href: crate::components::THEME_STYLESHEET_HREF }
        document::Stylesheet { href: "{view.tokens_href}" }
        crate::components::AppNavbar {
            destinations: crate::app_chrome::app_destinations(view.role),
            logo: view.logo.clone(),
        }
        main { id: "projects", class: "nav-theme",
            header { class: "page-header",
                h1 { "Projects" }
                p { a { class: "nav-btn nav-btn--primary", href: "/app/projects/new", "Add project" } }
            }
            { filter_row("Project status", &status_chips) }
            if !brand_chips.is_empty() {
                { filter_row("Project brand", &brand_chips) }
            }
            for (category, chips) in label_groups.iter() {
                div { key: "{category}",
                    span { "{category}" }
                    { filter_row(category, chips) }
                }
            }
            if let Some(error) = error.as_ref() {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            if is_empty {
                { empty_message(&view.sort, filters_active) }
            } else {
                DataTable {
                    columns,
                    sort,
                    base_path: "/app/projects".to_string(),
                    extra_query,
                    for row in view.rows.iter() {
                        tr { class: "project-row",
                            td { class: "project-code",
                                a {
                                    class: "nav-link",
                                    href: "/app/projects/{row.code}",
                                    "data-action": "view",
                                    "aria-label": "View details for {row.name}",
                                    "{row.code}"
                                }
                            }
                            td { class: "project-name", "{row.name}" }
                            td { class: "project-brand", "{row.brand_name}" }
                            td { class: "project-status",
                                span {
                                    class: "{row.lifecycle_class}",
                                    title: "{row.lifecycle_title}",
                                    "{row.lifecycle_label}"
                                }
                            }
                            if show_labels {
                                td { class: "project-labels",
                                    { row.labels.iter().map(|label| label.name.as_str()).collect::<Vec<_>>().join(", ") }
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

    fn owned(statuses: &[&str]) -> Vec<String> {
        statuses.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn label_filters_match_any_value_within_a_category_and_every_category() {
        let divorce = LabelFacet {
            category: "Project type".into(),
            name: "Divorce".into(),
            id: "a".into(),
        };
        let annulment = LabelFacet {
            category: "Project type".into(),
            name: "Annulment".into(),
            id: "b".into(),
        };
        let filed = LabelFacet {
            category: "Court filing".into(),
            name: "Filed".into(),
            id: "c".into(),
        };
        let selected = [&divorce, &annulment, &filed];
        assert!(labels_match(&[divorce.clone(), filed.clone()], &selected));
        assert!(labels_match(&[annulment.clone(), filed.clone()], &selected));
        assert!(!labels_match(
            &[divorce.clone(), annulment.clone()],
            &selected
        ));
        assert!(!labels_match(std::slice::from_ref(&filed), &selected));
        assert!(labels_match(&[], &[]));
    }

    #[test]
    fn no_status_query_selects_all_open_projects() {
        assert_eq!(selected_statuses(None), owned(&["open", "pitch", "active"]));
    }

    #[test]
    fn status_query_is_case_insensitive_canonically_ordered_and_ignores_unknowns() {
        assert_eq!(
            selected_statuses(Some("Closed, pitch,bogus")),
            owned(&["pitch", "closed"])
        );
        assert!(selected_statuses(Some(NO_STATUS)).is_empty());
    }

    #[test]
    fn toggling_round_trips_through_the_query() {
        let default = selected_statuses(None);
        let with_closed = toggle_status(&default, "closed");
        assert_eq!(with_closed, owned(&["open", "pitch", "active", "closed"]));
        assert_eq!(
            status_query(&with_closed),
            vec![("status".to_string(), "open,pitch,active,closed".to_string())]
        );
        assert!(status_query(&toggle_status(&with_closed, "closed")).is_empty());
        let none = toggle_status(&owned(&["active"]), "active");
        assert_eq!(selected_statuses(Some(&status_query(&none)[0].1)), none);
    }

    #[test]
    fn brand_filter_is_case_insensitive_deduplicated_and_absent_means_all() {
        assert!(selected_filter_keys(None).is_empty());
        assert!(selected_filter_keys(Some("")).is_empty());
        assert_eq!(
            selected_filter_keys(Some(" Neon , neon,delete-your-data")),
            owned(&["neon", "delete-your-data"])
        );
    }

    #[test]
    fn brand_toggling_round_trips_through_the_query() {
        let with_neon = toggle_key(&[], "neon");
        assert_eq!(with_neon, owned(&["neon"]));
        let with_both = toggle_key(&with_neon, "Vesta");
        assert_eq!(
            brand_query(&with_both),
            vec![("brand".to_string(), "neon,vesta".to_string())]
        );
        assert_eq!(toggle_key(&with_both, "vesta"), with_neon);
        assert!(brand_query(&toggle_key(&with_neon, "neon")).is_empty());
    }

    #[test]
    fn filter_queries_carry_both_filters_and_compose_with_the_sort() {
        let statuses = owned(&["pitch", "closed"]);
        let brands = owned(&["neon"]);
        assert_eq!(
            list_href("name", &filters_query(&statuses, &brands, &[])),
            "/app/projects?brand=neon&status=pitch,closed&sort=name"
        );
        assert_eq!(list_href("name", &[]), "/app/projects?sort=name");
        assert_eq!(list_href("", &[]), "/app/projects");
    }

    #[test]
    fn filter_rows_are_labelled_groups_with_aria_current_chips() {
        let chips = vec![
            // The selected chip's href drops its own value; the other chip's
            // adds it — both keep the sort.
            FilterChip {
                key: "neon".into(),
                label: "Neon".into(),
                selected: true,
                href: "/app/projects?sort=name".into(),
            },
            FilterChip {
                key: "vesta".into(),
                label: "Vesta".into(),
                selected: false,
                href: "/app/projects?brand=vesta&sort=name".into(),
            },
        ];
        let html = dioxus_ssr::render_element(filter_row("Project brand", &chips));
        assert!(html.contains(r#"role="group""#), "{html}");
        assert!(html.contains(r#"aria-label="Project brand""#), "{html}");
        assert!(html.contains(r#"aria-current="true""#), "{html}");
        assert!(html.contains(r#"href="/app/projects?sort=name""#), "{html}");
        assert!(
            html.contains(r#"href="/app/projects?brand=vesta&#38;sort=name""#),
            "{html}"
        );
    }

    #[test]
    fn a_filtered_empty_list_offers_clear_filters() {
        let html = dioxus_ssr::render_element(empty_message("name", true));
        assert!(html.contains("No projects match these filters."), "{html}");
        assert!(html.contains("Clear filters"), "{html}");
        assert!(html.contains(r#"href="/app/projects?sort=name""#), "{html}");
        let unfiltered = dioxus_ssr::render_element(empty_message("", false));
        assert!(unfiltered.contains("No projects match. "), "{unfiltered}");
        assert!(unfiltered.contains("/app/projects/new"), "{unfiltered}");
        assert!(!unfiltered.contains("Clear filters"), "{unfiltered}");
    }
}
