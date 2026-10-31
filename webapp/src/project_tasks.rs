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
/// ids to display names.
#[cfg(feature = "server")]
fn task_rows(
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
            due_on: row.due_on.map(|date| date.to_string()),
        })
        .collect()
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
    let tasks = store::tasks::list_for_project(&pg, project.id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let assignee_ids: Vec<uuid::Uuid> = tasks.iter().filter_map(|row| row.assignee_id).collect();
    let names: std::collections::HashMap<uuid::Uuid, String> =
        store::persons::find_by_ids(&surreal, &assignee_ids)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?
            .into_iter()
            .map(|person| (person.id, person.name))
            .collect();
    Ok(task_rows(&tasks, &names))
}

#[cfg(test)]
mod tests {
    use super::TaskRow;
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
        let tasks = [a_task(
            store::tasks::TaskState::InProgress,
            Some(Uuid::nil()),
        )];

        let rows: Vec<TaskRow> = super::task_rows(&tasks, &HashMap::new());

        assert_eq!(rows[0].state, "in_progress");
        assert_eq!(rows[0].assignee, None);
    }
}
