//! The workbench's upcoming Deadlines table across the caller's visible open
//! matters. Court appearances stay on the Project calendar; this table lists
//! obligations from today forward.

use chrono::{DateTime, FixedOffset};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

pub const DEADLINES_PER_PAGE: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    id: String,
    project_code: String,
    project_name: String,
    title: String,
    due: DateTime<FixedOffset>,
    civil_label: Option<String>,
    authority: String,
    notes: String,
}

impl Entry {
    /// The due date alone, for the Active list's "Next due" column.
    pub fn due_label(&self) -> String {
        self.civil_label
            .clone()
            .unwrap_or_else(|| self.due.format("%Y-%m-%d %H:%M %:z").to_string())
    }
}

/// Upcoming open Deadlines on visible open matters, fetched a page at a time.
#[cfg(feature = "server")]
pub(super) async fn load(
    projects: &[store::projects::Project],
    today: chrono::NaiveDate,
    page: usize,
) -> Result<(Vec<Entry>, bool), ServerFnError> {
    let open: std::collections::HashMap<uuid::Uuid, &store::projects::Project> = projects
        .iter()
        .filter(|project| project.status == "open")
        .map(|project| (project.id, project))
        .collect();
    let ids: Vec<uuid::Uuid> = open.keys().copied().collect();
    let offset = page.saturating_sub(1).saturating_mul(DEADLINES_PER_PAGE);
    let page_rows = store::project_deadlines::upcoming_page_for_projects(
        &ids,
        today,
        i64::try_from(DEADLINES_PER_PAGE).unwrap_or(10),
        i64::try_from(offset).unwrap_or(i64::MAX),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "dashboard: open deadlines failed");
        dioxus_fullstack_core::FullstackContext::commit_http_status(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            None,
        );
        ServerFnError::new("Could not load Deadlines")
    })?;
    let mut entries = Vec::with_capacity(page_rows.rows.len());
    for row in page_rows.rows {
        let Some(project) = open.get(&row.project_id) else {
            continue;
        };
        let due = DateTime::parse_from_rfc3339(&row.due_at)
            .map_err(|_| ServerFnError::new("A Deadline has an unreadable due date"))?;
        entries.push(Entry {
            id: row.id.to_string(),
            project_code: project.code.clone(),
            project_name: project.name.clone(),
            title: row.title.unwrap_or_else(|| {
                if row.calendar_role == "hearing" {
                    "Hearing".into()
                } else {
                    "Deadline".into()
                }
            }),
            due: row
                .due_on
                .as_ref()
                .map_or(due, |day| due.with_timezone(&day.timezone).fixed_offset()),
            civil_label: row
                .due_on
                .as_ref()
                .map(store::project_deadlines::DueOn::display_date),
            authority: if row.authority.trim().is_empty() {
                crate::project_deadlines::authority_kind_label(&row.authority_kind).into()
            } else {
                row.authority
            },
            notes: notes(row.description.unwrap_or_default()),
        });
    }
    Ok((entries, page_rows.has_next))
}

/// A proposal's description is JSON; show only the note a lawyer wrote.
#[cfg(feature = "server")]
fn notes(description: String) -> String {
    serde_json::from_str::<serde_json::Value>(&description)
        .ok()
        .filter(|value| value.get("deadline_proposal").is_some())
        .map_or(description, |value| {
            value["notes"].as_str().unwrap_or_default().to_owned()
        })
}

#[component]
pub fn WorkbenchDeadlines(
    entries: Vec<Entry>,
    page: u64,
    has_next: bool,
    status: String,
    sort: String,
    dir: String,
    project_page: u64,
) -> Element {
    rsx! {
        section { id: "workbench-deadlines", class: "project-deadlines", aria_labelledby: "workbench-deadlines-title",
            h2 { id: "workbench-deadlines-title", "Upcoming deadlines" }
            if entries.is_empty() {
                p { class: "nav-muted", "No upcoming Deadlines on your open matters." }
            } else {
                div { class: "nav-table-wrap",
                    table { class: "nav-table upcoming-deadlines-table",
                        thead { tr {
                            th { scope: "col", "Due" }
                            th { scope: "col", "Deadline" }
                            th { scope: "col", "Project" }
                            th { scope: "col", "Authority" }
                            th { scope: "col", "Notes" }
                        } }
                        tbody {
                            for entry in entries.iter() {
                                tr { key: "{entry.id}",
                                    td { "{entry.due_label()}" }
                                    td { a { href: "/app/projects/{entry.project_code}/deadlines#deadline-{entry.id}", "{entry.title}" } }
                                    td { a { href: "/app/projects/{entry.project_code}", "{entry.project_name}" } }
                                    td { "{entry.authority}" }
                                    td { "{entry.notes}" }
                                }
                            }
                        }
                    }
                }
                DeadlinePagination { page, has_next, status, sort, dir, project_page }
            }
        }
    }
}

#[component]
fn DeadlinePagination(
    page: u64,
    has_next: bool,
    status: String,
    sort: String,
    dir: String,
    project_page: u64,
) -> Element {
    if page <= 1 && !has_next {
        return rsx! {};
    }
    let page = page.max(1);
    let href = |target: u64| {
        format!(
            "/app/lawyer?status={status}&sort={sort}&dir={dir}&page={project_page}&deadline_page={target}"
        )
    };
    let previous = href(page.saturating_sub(1));
    let next = href(page.saturating_add(1));
    let summary = format!("Page {page}");
    rsx! {
        nav { class: "nav-pagination", aria_label: "Upcoming deadline pagination",
            if page > 1 {
                a { class: "nav-pagination__link", href: "{previous}", "Previous" }
            } else {
                span { class: "nav-pagination__link is-disabled", aria_disabled: "true", "Previous" }
            }
            span { class: "nav-pagination__status", aria_current: "page", "{summary}" }
            if has_next {
                a { class: "nav-pagination__link", href: "{next}", "Next" }
            } else {
                span { class: "nav-pagination__link is-disabled", aria_disabled: "true", "Next" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, due: &str) -> Entry {
        Entry {
            id: id.into(),
            project_code: "synthetic-matter".into(),
            project_name: "Synthetic matter".into(),
            title: format!("Due {id}"),
            due: DateTime::parse_from_rfc3339(due).unwrap(),
            civil_label: None,
            authority: "Synthetic scheduling order".into(),
            notes: "Moves to May 6 if the order is entered.".into(),
        }
    }

    #[test]
    fn upcoming_deadline_table_orders_and_links_rows() {
        let entries = vec![
            entry("a", "2028-05-05T12:00:00Z"),
            entry("b", "2028-05-20T12:00:00Z"),
        ];
        let html = dioxus_ssr::render_element(rsx! {
            WorkbenchDeadlines {
                entries,
                page: 1,
                has_next: true,
                status: "open".to_string(),
                sort: "project".to_string(),
                dir: "asc".to_string(),
                project_page: 2,
            }
        });
        for expected in [
            "Upcoming deadlines",
            "href=\"/app/projects/synthetic-matter/deadlines#deadline-a\"",
            "href=\"/app/lawyer?status=open&#38;sort=project&#38;dir=asc&#38;page=2&#38;deadline_page=2\"",
            "Synthetic scheduling order",
            "Moves to May 6 if the order is entered.",
        ] {
            assert!(html.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn no_open_deadlines_names_its_empty_state() {
        let html = dioxus_ssr::render_element(rsx! { WorkbenchDeadlines {
            entries: vec![], page: 1, has_next: false,
            status: "open".to_string(), sort: "date".to_string(),
            dir: "asc".to_string(), project_page: 1,
        } });
        assert!(html.contains("No upcoming Deadlines on your open matters."));
    }
}
