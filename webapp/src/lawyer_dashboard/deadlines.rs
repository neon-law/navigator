//! The workbench's Deadlines section: every open Deadline across the caller's
//! visible open matters, grouped by urgency. Court appearances stay on the
//! Project calendar; this section lists obligations.

use chrono::{DateTime, Days, FixedOffset, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

const BANDS: [&str; 5] = ["Overdue", "Today", "Next 7 days", "Next 30 days", "Later"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    id: String,
    project_code: String,
    project_name: String,
    title: String,
    due: DateTime<FixedOffset>,
    authority: String,
    notes: String,
}

impl Entry {
    /// The due date alone, for the Active list's "Next due" column.
    pub fn due_label(&self) -> String {
        self.due.format("%Y-%m-%d").to_string()
    }

    pub fn project_code(&self) -> &str {
        &self.project_code
    }
}

fn band(due: DateTime<FixedOffset>, now: DateTime<Utc>) -> &'static str {
    if due.to_utc() < now {
        return "Overdue";
    }
    let today = now.with_timezone(due.offset()).date_naive();
    let day = due.date_naive();
    if day == today {
        "Today"
    } else if today
        .checked_add_days(Days::new(7))
        .is_some_and(|end| day <= end)
    {
        "Next 7 days"
    } else if today
        .checked_add_days(Days::new(30))
        .is_some_and(|end| day <= end)
    {
        "Next 30 days"
    } else {
        "Later"
    }
}

/// Open Deadlines on `projects`' open matters, one batched read, earliest first.
#[cfg(feature = "server")]
pub(super) async fn load(
    db: &store::surreal::SurrealDb,
    projects: &[store::projects::Project],
) -> Result<Vec<Entry>, ServerFnError> {
    let open: std::collections::HashMap<uuid::Uuid, &store::projects::Project> = projects
        .iter()
        .filter(|project| project.status == "open")
        .map(|project| (project.id, project))
        .collect();
    let ids: Vec<uuid::Uuid> = open.keys().copied().collect();
    let rows = store::project_deadlines::open_for_projects(db, &ids)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "dashboard: open deadlines failed");
            dioxus_fullstack_core::FullstackContext::commit_http_status(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                None,
            );
            ServerFnError::new("Could not load Deadlines")
        })?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
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
            due,
            authority: if row.authority.trim().is_empty() {
                crate::project_deadlines::authority_kind_label(&row.authority_kind).into()
            } else {
                row.authority
            },
            notes: notes(row.description.unwrap_or_default()),
        });
    }
    entries.sort_by(|a, b| (a.due.to_utc(), &a.id).cmp(&(b.due.to_utc(), &b.id)));
    Ok(entries)
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
pub fn WorkbenchDeadlines(entries: Vec<Entry>, now: DateTime<Utc>) -> Element {
    rsx! {
        section { id: "workbench-deadlines", class: "project-deadlines", aria_labelledby: "workbench-deadlines-title",
            h2 { id: "workbench-deadlines-title", "Deadlines" }
            if entries.is_empty() {
                p { class: "nav-muted", "No open Deadlines on your open matters." }
            } else {
                for label in BANDS {
                    if entries.iter().any(|entry| band(entry.due, now) == label) {
                        h3 { class: if label == "Overdue" { "is-overdue" } else { "" }, "{label}" }
                        ul {
                            for entry in entries.iter().filter(|entry| band(entry.due, now) == label) {
                                DeadlineRow { key: "{entry.id}", entry: entry.clone() }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn DeadlineRow(entry: Entry) -> Element {
    let due = entry.due.format("%Y-%m-%d %H:%M %:z").to_string();
    rsx! {
        li {
            "{due} · "
            a { href: "/app/projects/{entry.project_code}/deadlines#deadline-{entry.id}", "{entry.title}" }
            " · "
            a { href: "/app/projects/{entry.project_code}", "{entry.project_name}" }
            if !entry.authority.is_empty() { p { "{entry.authority}" } }
            if !entry.notes.is_empty() { p { "{entry.notes}" } }
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
            authority: "Synthetic scheduling order".into(),
            notes: "Moves to May 6 if the order is entered.".into(),
        }
    }

    #[test]
    fn deadlines_group_by_urgency_and_link_back_to_the_matter() {
        let now = "2028-05-01T12:00:00Z".parse().unwrap();
        let entries = vec![
            entry("a", "2028-04-28T12:00:00Z"),
            entry("b", "2028-05-01T20:00:00Z"),
            entry("c", "2028-05-05T12:00:00Z"),
            entry("d", "2028-05-20T12:00:00Z"),
            entry("e", "2029-01-01T12:00:00Z"),
        ];
        let html = dioxus_ssr::render_element(rsx! { WorkbenchDeadlines { entries, now } });
        let order: Vec<usize> = ["Overdue", "Today", "Next 7 days", "Next 30 days", "Later"]
            .iter()
            .map(|label| {
                html.find(&format!(">{label}<"))
                    .unwrap_or_else(|| panic!("no {label}"))
            })
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]));
        for expected in [
            "href=\"/app/projects/synthetic-matter/deadlines#deadline-a\"",
            "Synthetic scheduling order",
            "Moves to May 6 if the order is entered.",
        ] {
            assert!(html.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn no_open_deadlines_names_its_empty_state() {
        let html = dioxus_ssr::render_element(
            rsx! { WorkbenchDeadlines { entries: vec![], now: Utc::now() } },
        );
        assert!(html.contains("No open Deadlines on your open matters."));
    }
}
