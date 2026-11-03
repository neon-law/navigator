//! The Tasks tab's rows: a matter's assignable work with state and assignee.
//!
//! The dashboard's read seam for [`store::tasks`], beside the write doors in
//! the portal's task API. Reached only through `store::access::matter_viewer`
//! — a participation row is required of every tier (ENG-81), and the Tasks tab
//! is a firm-side surface, so a client-side participant, a clerk, an
//! unassigned lawyer, Owner and Admin included, all get the non-disclosing
//! `404`. A direct hit on the generated `#[server]` endpoint defaults to the
//! least-privileged tier, so the lawyer-tier check here is defense in depth,
//! not a duplicate of the router's gate.
//!
//! Rows are plain scalars — no `store` type crosses to the wasm build — and
//! the assignee is resolved to a display name from the person directory, never
//! carried as a bare id.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Choice, Empty, Field, FormCard, Heading};
use crate::people::ViewerRole;

/// One task row for the Tasks tab.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskRow {
    pub id: String,
    pub title: String,
    pub body: Option<String>,
    /// The stored word: `todo`, `in_progress`, or `done` — the same vocabulary
    /// the task API's transition command speaks.
    pub state: String,
    /// The assignee's display name, when the task carries one.
    pub assignee: Option<String>,
    /// The assignee's person id, when there is one — the value the row's
    /// assign control preselects. The display name is the label; this is the
    /// wire value.
    pub assignee_id: Option<String>,
    /// ISO `yyyy-mm-dd`, a scheduling aid — never a docket obligation.
    pub due_on: Option<String>,
}

/// Commit the `404` the matter surface answers for a caller the matter does
/// not exist for, and fail the server fn with it — hidden, never refused.
#[cfg(feature = "server")]
fn hidden_matter() -> ServerFnError {
    dioxus_fullstack_core::FullstackContext::commit_http_status(
        axum::http::StatusCode::NOT_FOUND,
        None,
    );
    ServerFnError::new("no such matter")
}

/// Map the task store's rows onto the wasm-safe view rows, resolving assignee
/// ids to display names. Shared by the standalone read seam and the dashboard
/// server fn, so the two read paths cannot drift.
#[cfg(feature = "server")]
pub(crate) fn task_rows(
    tasks: &[store::tasks::ProjectTask],
    names: &std::collections::HashMap<uuid::Uuid, String>,
) -> Vec<TaskRow> {
    tasks
        .iter()
        .map(|row| TaskRow {
            id: row.id.to_string(),
            title: row.title.clone(),
            body: row.body.clone(),
            state: row.state.as_str().to_string(),
            assignee: row.assignee_id.and_then(|id| names.get(&id).cloned()),
            assignee_id: row.assignee_id.map(|id| id.to_string()),
            due_on: row.due_on.map(|date| date.to_string()),
        })
        .collect()
}

/// Fetch the matter's tasks: open work first by due date, then done work
/// last, each with its state and assignee's name. The store access and
/// PostgreSQL handle are the caller's — both read paths have already asked
/// `store::access::matter_viewer` by the time they get here.
#[cfg(feature = "server")]
pub(crate) async fn read_rows(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    project_id: uuid::Uuid,
) -> Result<Vec<TaskRow>, ServerFnError> {
    let tasks = store::tasks::list_for_project(pg, project_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let assignee_ids: Vec<uuid::Uuid> = tasks.iter().filter_map(|row| row.assignee_id).collect();
    let names: std::collections::HashMap<uuid::Uuid, String> =
        store::persons::find_by_ids(surreal, &assignee_ids)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?
            .into_iter()
            .map(|person| (person.id, person.name))
            .collect();
    Ok(task_rows(&tasks, &names))
}

/// Fetch the matter's tasks for the Tasks tab: open work first by due date,
/// then done work last, each with its state and assignee's name.
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
#[server]
pub async fn get_matter_tasks() -> Result<Vec<TaskRow>, ServerFnError> {
    let axum::extract::Path(code) =
        dioxus_fullstack_core::FullstackContext::extract::<axum::extract::Path<String>, _>()
            .await?;
    let role = dioxus_fullstack_core::FullstackContext::extract::<axum::Extension<ViewerRole>, _>()
        .await
        .map(|axum::Extension(role)| role)
        .unwrap_or_default();
    if !role.is_lawyer_tier() {
        return Err(hidden_matter());
    }
    let crate::portal_project_list::PersonId(person_id) =
        dioxus_fullstack_core::FullstackContext::extract::<
            axum::Extension<crate::portal_project_list::PersonId>,
            _,
        >()
        .await
        .map(|axum::Extension(id)| id)
        .unwrap_or_default();
    let person_id = person_id.and_then(|raw| raw.parse::<uuid::Uuid>().ok());

    let surreal = consume_context::<store::surreal::SurrealDb>();
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?
    else {
        return Err(hidden_matter());
    };

    let store_role = match role {
        ViewerRole::Owner => store::persons::Role::Owner,
        ViewerRole::Admin => store::persons::Role::Admin,
        ViewerRole::Lawyer => store::persons::Role::Lawyer,
        ViewerRole::Clerk => store::persons::Role::Clerk,
        ViewerRole::Client => store::persons::Role::Client,
    };
    let viewer = store::access::matter_viewer(&surreal, person_id, store_role, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(hidden_matter());
    }

    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| crate::lawyer_project_detail::server_error("the task store is unavailable"))?;
    read_rows(&surreal, &pg, project.id).await
}

/// The human word for a task's stored state. The stored snake-case word is
/// the wire and command vocabulary (`store::tasks::TaskState::as_str`); this
/// is the panel's presentation label, never a second vocabulary on the wire.
fn state_label(state: &str) -> &'static str {
    match state {
        "in_progress" => "In progress",
        "done" => "Done",
        _ => "To do",
    }
}

/// The state chip's tone: work in motion reads as "coming up", finished work
/// as "done", and untouched work stays neutral.
fn state_tone(state: &str) -> BadgeTone {
    match state {
        "in_progress" => BadgeTone::Next,
        "done" => BadgeTone::Ready,
        _ => BadgeTone::Default,
    }
}

/// The one legal forward move from a state (`store::tasks::TaskState::
/// can_transition_to`), as the stored word the transition command speaks —
/// `None` when the task is `done`, which is a transition, never a delete.
fn next_state(state: &str) -> Option<(&'static str, &'static str)> {
    match state {
        "todo" => Some(("in_progress", "Start")),
        "in_progress" => Some(("done", "Complete")),
        _ => None,
    }
}

/// One choice a task's assign control can name, already resolved to
/// `(person id, display name)` — a firm-side participant of this matter.
pub type AssigneeChoice = (String, String);

/// The Tasks tab: the matter's assignable work, with the create form and the
/// per-row assign and transition controls. A Task is deliberately not a
/// deadline — deadlines are docket obligations and stay on Overview.
///
/// Every write is a native-form POST to the matter's task handlers, which
/// re-ask the same authorization this page was rendered under; the controls
/// render only what the command layer would honour.
#[component]
pub fn TasksTab(
    code: String,
    csrf: String,
    tasks: Vec<TaskRow>,
    assignees: Vec<AssigneeChoice>,
    /// The `?error=` flash a refused write redirected back with.
    #[props(default)]
    error: Option<String>,
) -> Element {
    let assignee_choices: Vec<Choice> =
        std::iter::once(Choice::new(String::new(), "Unassigned".to_string()))
            .chain(
                assignees
                    .iter()
                    .map(|(id, name)| Choice::new(id.clone(), name.clone())),
            )
            .collect();

    rsx! {
        section { class: "lawyer-detail__section lawyer-detail__tasks",
            h2 { "Tasks" }
            if let Some(error) = error {
                p { class: "nav-form-error", role: "alert", "{error}" }
            }
            if tasks.is_empty() {
                Empty {
                    title: "No tasks on this matter yet".to_string(),
                    description: Some(
                        "A task is assignable work with an owner and a state — create the first \
                         one below. Court deadlines are docket obligations and stay on Overview."
                            .to_string(),
                    ),
                }
            } else {
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { scope: "col", "Task" }
                                th { scope: "col", "Assignee" }
                                th { scope: "col", "Due" }
                                th { scope: "col", "State" }
                                th { scope: "col", "Actions" }
                            }
                        }
                        tbody {
                            for row in tasks.iter() {
                                tr {
                                    td {
                                        "{row.title}"
                                        if let Some(body) = row.body.as_ref() {
                                            p { "{body}" }
                                        }
                                    }
                                    td { "{row.assignee.clone().unwrap_or_else(|| \"—\".to_string())}" }
                                    td { "{row.due_on.clone().unwrap_or_else(|| \"—\".to_string())}" }
                                    td {
                                        Badge { tone: state_tone(&row.state), "{state_label(&row.state)}" }
                                    }
                                    td { class: "nav-table__end",
                                        if let Some((next, label)) = next_state(&row.state) {
                                            form {
                                                class: "lawyer-detail__inline-form",
                                                method: "post",
                                                action: "/app/projects/{code}/tasks/{row.id}/transition",
                                                input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                                                input { r#type: "hidden", name: "next_state", value: "{next}" }
                                                button {
                                                    class: "nav-btn nav-btn--secondary matter-task-transition",
                                                    r#type: "submit",
                                                    "{label}"
                                                }
                                            }
                                        }
                                        form {
                                            class: "lawyer-detail__inline-form",
                                            method: "post",
                                            action: "/app/projects/{code}/tasks/{row.id}/assign",
                                            input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                                            select {
                                                class: "nav-select matter-task-assignee",
                                                name: "assignee_id",
                                                "aria-label": "Assignee for {row.title}",
                                                for choice in assignee_choices.iter() {
                                                    option {
                                                        value: "{choice.value}",
                                                        selected: row.assignee_id.as_deref() == Some(choice.value.as_str()),
                                                        "{choice.label}"
                                                    }
                                                }
                                            }
                                            button {
                                                class: "nav-btn nav-btn--secondary matter-task-assign",
                                                r#type: "submit",
                                                "Assign"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            FormCard {
                title: "Add a task".to_string(),
                action: "/app/projects/{code}/tasks",
                submit_label: "Add task".to_string(),
                heading: Heading::H2,
                csrf_token: Some(csrf.clone()),
                fields: vec![
                    Field::text("Title", "title", "")
                        .required()
                        .help("Required. The work in one line — \u{201c}Serve the request for production\u{201d}."),
                    Field::textarea("Details", "body", "", 3).help("Optional."),
                    Field::select("Assignee", "assignee_id", assignee_choices, Some(String::new()))
                        .help("Only people assigned to this matter can hold a task."),
                    Field::input("Due date", "due_on", "", "date")
                        .help("Optional. A scheduling aid — a missed task is not a malpractice event; a missed deadline is."),
                ],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use std::collections::HashMap;
    use store::tasks::ProjectTask;
    use uuid::Uuid;

    fn a_task(state: store::tasks::TaskState, assignee_id: Option<Uuid>) -> ProjectTask {
        ProjectTask {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            title: "Serve the request for production".to_string(),
            body: None,
            state,
            assignee_id,
            due_on: Some(
                chrono::NaiveDate::from_ymd_opt(2026, 11, 9).expect("invariant: fixed date"),
            ),
            inserted_at: DateTime::default(),
            updated_at: DateTime::default(),
        }
    }

    #[test]
    fn rows_carry_the_stored_state_word_and_resolved_assignee_name() {
        let assignee = Uuid::max();
        let tasks = [
            a_task(store::tasks::TaskState::Todo, Some(assignee)),
            a_task(store::tasks::TaskState::Done, None),
        ];
        let names = HashMap::from([(assignee, "A. Rivera".to_string())]);

        let rows = super::task_rows(&tasks, &names);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].state, "todo");
        assert_eq!(rows[0].assignee.as_deref(), Some("A. Rivera"));
        assert_eq!(rows[0].due_on.as_deref(), Some("2026-11-09"));
        assert_eq!(rows[1].state, "done");
        assert_eq!(rows[1].assignee, None);
    }

    #[test]
    fn a_row_whose_assignee_is_not_in_the_directory_carries_no_name() {
        let departed = Uuid::nil();
        let tasks = [a_task(store::tasks::TaskState::InProgress, Some(departed))];

        let rows: Vec<TaskRow> = super::task_rows(&tasks, &HashMap::new());

        assert_eq!(rows[0].state, "in_progress");
        // The stored assignment survives a participant leaving the matter —
        // the row just renders an em dash until someone reassigns.
        assert_eq!(rows[0].assignee, None);
        assert_eq!(
            rows[0].assignee_id.as_deref(),
            Some("00000000-0000-0000-0000-000000000000")
        );
    }

    fn a_row(state: &str, assignee_id: Option<&str>) -> TaskRow {
        TaskRow {
            id: "00000000-0000-0000-0000-00000000aa00".to_string(),
            title: "Serve the request for production".to_string(),
            body: None,
            state: state.to_string(),
            assignee: assignee_id.map(str::to_string),
            assignee_id: assignee_id.map(str::to_string),
            due_on: Some("2026-11-09".to_string()),
        }
    }

    #[test]
    fn the_human_labels_stay_off_the_wire_vocabulary() {
        assert_eq!(super::state_label("todo"), "To do");
        assert_eq!(super::state_label("in_progress"), "In progress");
        assert_eq!(super::state_label("done"), "Done");
    }

    #[test]
    fn done_has_no_forward_transition() {
        assert_eq!(super::next_state("todo"), Some(("in_progress", "Start")));
        assert_eq!(super::next_state("in_progress"), Some(("done", "Complete")));
        assert_eq!(super::next_state("done"), None);
    }

    #[test]
    fn an_empty_tab_explains_itself_and_offers_the_create_form() {
        let html = dioxus_ssr::render_element(rsx! {
            TasksTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                tasks: vec![],
                assignees: vec![],
            }
        });

        assert!(html.contains("No tasks on this matter yet"), "{html}");
        // The next action is the form itself, carried by the same section.
        assert!(
            html.contains(r#"action="/app/projects/acme/tasks""#),
            "{html}"
        );
        assert!(html.contains("Add task"), "{html}");
    }

    #[test]
    fn rows_render_their_state_badge_and_row_actions() {
        let html = dioxus_ssr::render_element(rsx! {
            TasksTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                tasks: vec![
                    a_row("todo", None),
                    a_row("in_progress", Some("00000000-0000-0000-0000-0000000000aa")),
                    a_row("done", None),
                ],
                assignees: vec![(
                    "00000000-0000-0000-0000-0000000000aa".to_string(),
                    "A. Rivera".to_string(),
                )],
            }
        });

        // The stored vocabulary is the wire word (the hidden transition
        // input carries it); the human label is what the badge shows.
        assert!(
            html.contains(">In progress</span>"),
            "the badge shows the human label: {html}"
        );
        assert!(!html.contains(">in_progress<"), "{html}");
        // One forward transition per open row, none for the done row.
        assert_eq!(html.matches("matter-task-transition").count(), 2, "{html}");
        assert!(html.contains("Complete"), "{html}");
        assert!(
            html.contains(r#"name="next_state" value="in_progress""#),
            "{html}"
        );
        assert!(html.contains(r#"name="next_state" value="done""#), "{html}");
        // The assign control posts the row's own assign door, names its
        // assignee for the screen reader, and preselects the held assignee.
        // (The token boundary matters: `matter-task-assignee` also contains
        // `matter-task-assign`.)
        assert_eq!(html.matches("matter-task-assign\"").count(), 3, "{html}");
        assert!(
            html.contains("Assignee for Serve the request for production"),
            "{html}"
        );
        assert!(
            html.contains(r#"value="00000000-0000-0000-0000-0000000000aa" selected=true"#),
            "{html}"
        );
        // The assignee picker offers exactly the matter's participants.
        assert!(html.contains("A. Rivera"), "{html}");
        assert!(html.contains("Unassigned"), "{html}");
        // Every row action clears the CSRF guard.
        assert!(html.contains(r#"name="_csrf""#), "{html}");
    }

    #[test]
    fn task_details_are_visible_without_hover_or_focus() {
        let mut row = a_row("todo", None);
        row.body = Some("Send the signed request to counsel.".to_string());
        let html = dioxus_ssr::render_element(rsx! {
            TasksTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                tasks: vec![row],
                assignees: vec![],
            }
        });

        assert!(
            html.contains(">Send the signed request to counsel.</p>"),
            "{html}"
        );
        assert!(!html.contains("title=\"Send the signed request"), "{html}");
    }

    #[test]
    fn a_refused_write_rides_back_as_the_error_flash() {
        let html = dioxus_ssr::render_element(rsx! {
            TasksTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                tasks: vec![],
                assignees: vec![],
                error: Some("That task is not on this matter.".to_string()),
            }
        });

        assert!(
            html.contains(r#"role="alert""#),
            "the refusal must be announced: {html}"
        );
        assert!(html.contains("That task is not on this matter."), "{html}");
    }
}
