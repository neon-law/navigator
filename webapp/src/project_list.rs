//! The firm-side projects list at `/app/projects`: every matter the reader
//! may reach, as code, name, Brand, and status.
//!
//! The list carries only what a reader scans for. Entity, dates, and the last
//! commit live on the matter's show page at `/app/projects/{code}`, which is
//! where every row links.
//!
//! A Lawyer sees the matters they participate in firm-side. An Admin also
//! sees every matter in a Firm they administer, including one nobody is on
//! yet, and an Owner sees every matter (see [`accessible_projects`]). No Brand
//! or Firm tab narrows the list.
//!
//! Status is the matter-lifecycle pill (`store::projects::matter_lifecycle`):
//! `pitch`, `active`, or `closed`, with an archived matter reading `closed`.
//! The `?status=` filter is a multi-select over those three, comma-separated;
//! with no `?status=` the list shows `pitch` and `active`.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Column, DataTable, SortState};
use crate::people::ViewerRole;
#[cfg(feature = "server")]
use crate::portal_project_list::PersonId;

/// Every status the filter offers, in lifecycle order.
pub const PROJECT_STATUSES: [&str; 3] = ["pitch", "active", "closed"];

/// The statuses selected when the request names none.
pub const DEFAULT_PROJECT_STATUSES: [&str; 2] = ["pitch", "active"];

/// The `?status=` value that selects no status. Any value the filter does
/// not offer selects nothing, so this one only needs to be stable.
const NO_STATUS: &str = "none";

/// One matter row, in a wasm-safe shape (plain fields — no `store` types cross
/// to the client build).
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub code: String,
    pub name: String,
    /// The Brand's display name, or the raw key when no `brand` row matches.
    pub brand_name: String,
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
) -> ProjectRow {
    let status = if m.status.eq_ignore_ascii_case("archived") {
        "closed"
    } else {
        m.status.as_str()
    };
    let (missing_onboarding, missing_offboarding_letter) =
        store::projects::matter_flags(has_engagement, status, has_closing);
    let lifecycle =
        store::projects::matter_lifecycle(status, missing_onboarding, missing_offboarding_letter);
    ProjectRow {
        brand_name,
        code: m.code,
        name: m.name,
        lifecycle_class: lifecycle.class().to_string(),
        lifecycle_label: lifecycle.label().to_string(),
        lifecycle_title: lifecycle.title().to_string(),
    }
}

/// The matters the reader may reach from this list. An Owner holds every Firm
/// capability, so every matter. An Admin also staffs matters nobody is on yet,
/// so every matter in a Firm whose membership they administer, and every
/// matter no Firm owns. Everyone else sees the matters they participate in
/// firm-side.
#[cfg(feature = "server")]
async fn accessible_projects(
    surreal: &store::surreal::SurrealDb,
    person_id: Option<uuid::Uuid>,
    role: store::persons::Role,
) -> Result<Vec<store::projects::Project>, ServerFnError> {
    use store::persons::Role;

    if role == Role::Owner {
        return store::projects::all(surreal).await.map_err(loader_error);
    }
    let mut matters = store::access::visible_projects_as_lawyer(surreal, person_id, role)
        .await
        .map_err(loader_error)?;
    if role != Role::Admin {
        return Ok(matters);
    }
    let mut administered = std::collections::HashMap::new();
    for project in store::projects::all(surreal).await.map_err(loader_error)? {
        if matters.iter().any(|m| m.id == project.id) {
            continue;
        }
        let Some(firm_id) = project.firm_id else {
            matters.push(project);
            continue;
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = administered.entry(firm_id) {
            let decision = store::firm_capability::resolve(
                surreal,
                role,
                person_id,
                firm_id,
                store::firm_capability::FirmCapability::ManageMembership,
            )
            .await
            .map_err(loader_error)?;
            entry.insert(decision.is_allowed());
        }
        if administered[&firm_id] {
            matters.push(project);
        }
    }
    Ok(matters)
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
    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let matters = accessible_projects(&surreal, person_id, store_role).await?;
    let mut brands = store::brands::system_wide(&surreal)
        .await
        .map_err(loader_error)?;
    brands.extend(
        store::brands::all_firm_scoped(&surreal)
            .await
            .map_err(loader_error)?,
    );
    // A failed lifecycle lookup propagates rather than badging every matter
    // as a pitch.
    let (has_engagement, has_closing) = store::projects::matter_lifecycle_sets(&surreal, &matters)
        .await
        .map_err(loader_error)?;

    let mut rows: Vec<ProjectRow> = matters
        .into_iter()
        .map(|m| {
            let brand_name = brands
                .iter()
                .find(|b| b.key == m.brand)
                .map_or(m.brand.as_str(), |b| b.name.as_str())
                .to_string();
            let has_eng = has_engagement.contains(&m.id);
            let has_close = has_closing.contains(&m.id);
            project_row(brand_name, m, has_eng, has_close)
        })
        .filter(|row| statuses.contains(&row.lifecycle_label))
        .collect();
    sort_rows(&mut rows, &parse_sort(&sort));

    Ok(ProjectListView {
        firm_name: crate::app_chrome::firm_name_from_context().await,
        rows,
        sort,
        role,
        statuses,
        logo: crate::app_chrome::app_logo_from_context().await,
        tokens_href: crate::app_chrome::app_tokens_href_from_context().await,
        error: query.error.filter(|message| !message.is_empty()),
    })
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
    let extra_query = status_query(&view.statuses);
    let toggles: Vec<(&str, bool, String)> = PROJECT_STATUSES
        .iter()
        .map(|status| {
            let selected = view.statuses.iter().any(|s| s == status);
            let href = list_href(
                &view.sort,
                &status_query(&toggle_status(&view.statuses, status)),
            );
            (*status, selected, href)
        })
        .collect();
    let columns = vec![
        Column::sortable("code", "Code"),
        Column::sortable("name", "Name"),
        Column::sortable("brand", "Brand"),
        Column::sortable("status", "Status"),
    ];
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
            nav { class: "nav-tabs", aria_label: "Project status",
                for (status, selected, href) in toggles {
                    a { key: "{status}",
                        class: if selected { "nav-tab is-active" } else { "nav-tab" },
                        href: "{href}",
                        "data-status": "{status}",
                        "aria-current": if selected { "true" } else { "false" },
                        "{status}"
                    }
                }
            }
            if let Some(error) = error.as_ref() {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            if is_empty {
                p { class: "projects-empty",
                    "No projects match. "
                    a { href: "/app/projects/new", "Add a project." }
                }
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
    fn no_status_query_selects_pitch_and_active() {
        assert_eq!(selected_statuses(None), owned(&["pitch", "active"]));
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
        assert_eq!(with_closed, owned(&["pitch", "active", "closed"]));
        assert_eq!(
            status_query(&with_closed),
            vec![("status".to_string(), "pitch,active,closed".to_string())]
        );
        assert!(status_query(&toggle_status(&with_closed, "closed")).is_empty());
        let none = toggle_status(&owned(&["active"]), "active");
        assert_eq!(selected_statuses(Some(&status_query(&none)[0].1)), none);
    }
}
