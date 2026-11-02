//! Native-form write doors for the matter dashboard's Tasks tab — the
//! browser half of the task commands, beside the JSON API in
//! [`crate::project_tasks_api`].
//!
//! The dashboard is a native-form surface: the Tasks tab's create, assign,
//! and transition controls POST here, and every handler redirects (303) back
//! to `?tab=tasks`, with an `?error=` flash when a command is refused. The
//! authorization, mirroring, and store commands are the JSON door's own —
//! both doors call the same `prepared_store`, so the two entry points cannot
//! drift. The flash copy refuses in the user's vocabulary: never the wire's
//! `TaskState` words, a forbidden UUID, or a store variant name.
use axum::{
    extract::{Form, Path, State},
    response::{IntoResponse, Redirect, Response},
    Extension,
};
use chrono::NaiveDate;
use serde::Deserialize;
use uuid::Uuid;

use crate::admin::{encode_query_value, not_found_response};
use crate::session::SessionData;

/// Back to the tab the write came from, success or refusal alike — the flash
/// parameter carries the outcome.
fn back_to_tasks(code: &str, flash: Option<&str>) -> Response {
    let mut url = format!("/app/projects/{code}?tab=tasks");
    if let Some(flash) = flash {
        url.push_str("&error=");
        url.push_str(&encode_query_value(flash));
    }
    Redirect::to(&url).into_response()
}

/// The create form on the Tasks tab: a title, an optional body, an optional
/// assignee chosen from the matter's own participants, and an optional due
/// date. The CSRF token travels with the form but is consumed by the session
/// middleware before this handler sees it, so it is not a field here.
#[derive(Deserialize)]
pub(crate) struct TaskCreateForm {
    #[serde(default)]
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) body: String,
    #[serde(default)]
    pub(crate) assignee_id: String,
    #[serde(default)]
    pub(crate) due_on: String,
}

/// `POST /app/projects/{code}/tasks` — create a task from the tab's form.
/// An empty assignee or due date means "none"; a malformed date or a title
/// that is only whitespace comes back as a flash, not a 500.
pub(crate) async fn create(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<TaskCreateForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !crate::admin::is_lawyer_tier(Some(&session)) {
        return not_found_response();
    }
    // The JSON door's own admission, verbatim: a firm-side viewer of this
    // matter or the non-disclosing 404.
    let viewer =
        store::access::matter_viewer(&surreal, session.person_id, session.role, project.id)
            .await
            .ok()
            .flatten();
    match viewer {
        Some(viewer) if viewer.is_firm_side() => {}
        _ => return not_found_response(),
    }

    let title = input.title.trim().to_string();
    let body = input.body.trim().to_string();
    let assignee_id = parse_assignee(&input.assignee_id);
    let due_on = parse_due_on(&input.due_on);
    if title.is_empty() {
        return back_to_tasks(&code, Some("Give the task a title."));
    }
    if !input.due_on.trim().is_empty() && due_on.is_none() {
        return back_to_tasks(
            &code,
            Some("Enter the due date as a calendar date, or leave it empty."),
        );
    }

    let Ok(pg) =
        crate::project_tasks_api::prepared_store(&surreal, "create", project.id, None).await
    else {
        return back_to_tasks(
            &code,
            Some("The task store is unavailable right now — try again shortly."),
        );
    };
    match store::tasks::create(
        &pg,
        project.id,
        person_id,
        &store::tasks::NewProjectTask {
            title,
            body: (!body.is_empty()).then_some(body),
            assignee_id,
            due_on,
        },
    )
    .await
    {
        Ok(_) => back_to_tasks(&code, None),
        Err(e) => back_to_tasks(&code, Some(&flash_for(&e))),
    }
}

/// The row action's assign control: names the person the task now belongs
/// to. An empty choice clears the assignment.
#[derive(Deserialize)]
pub(crate) struct TaskAssignForm {
    #[serde(default)]
    pub(crate) assignee_id: String,
}

/// `POST /app/projects/{code}/tasks/{task_id}/assign` — set or clear the
/// task's assignee.
pub(crate) async fn assign(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((code, task_id)): Path<(String, Uuid)>,
    Form(input): Form<TaskAssignForm>,
) -> Response {
    let gate = match row_action_gate(&surreal, session.as_ref(), &code, "assign", task_id).await {
        Ok(gate) => gate,
        Err(response) => return response,
    };
    let assignee_id = parse_assignee(&input.assignee_id);
    match store::tasks::assign(&gate.pg, task_id, gate.person_id, assignee_id).await {
        Ok(_) => back_to_tasks(&code, None),
        Err(e) => back_to_tasks(&code, Some(&flash_for(&e))),
    }
}

/// The row action's transition control: the one forward move the task's
/// current state offers.
#[derive(Deserialize)]
pub(crate) struct TaskTransitionForm {
    #[serde(default)]
    pub(crate) next_state: String,
}

/// `POST /app/projects/{code}/tasks/{task_id}/transition` — move the task to
/// the state the row action posted.
pub(crate) async fn transition(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((code, task_id)): Path<(String, Uuid)>,
    Form(input): Form<TaskTransitionForm>,
) -> Response {
    let gate = match row_action_gate(&surreal, session.as_ref(), &code, "transition", task_id).await
    {
        Ok(gate) => gate,
        Err(response) => return response,
    };
    // The row actions only ever offer a forward move, so the posted state is
    // one of the two targets; `todo` is never one — there is no move back.
    let next = match input.next_state.trim() {
        "in_progress" => store::tasks::TaskState::InProgress,
        "done" => store::tasks::TaskState::Done,
        _ => {
            return back_to_tasks(
                &code,
                Some("That task state is not one this dashboard offers."),
            );
        }
    };
    match store::tasks::transition(&gate.pg, task_id, gate.person_id, next).await {
        Ok(_) => back_to_tasks(&code, None),
        Err(e) => back_to_tasks(&code, Some(&flash_for(&e))),
    }
}

/// What the row-action doors share once the preamble passes: the acting
/// person and the prepared task store — mirrored for this matter's project.
struct RowActionGate {
    person_id: Uuid,
    pg: store::postgres::PostgresDb,
}

/// The shared preamble of the two row-action doors — session, matter gate,
/// firm side, prepared store. Every refusal is the same 404 an off-matter
/// lawyer already gets.
async fn row_action_gate(
    surreal: &store::surreal::SurrealDb,
    session: Option<&Extension<SessionData>>,
    code: &str,
    operation: &'static str,
    task_id: Uuid,
) -> Result<RowActionGate, Response> {
    let Some(Extension(session)) = session else {
        return Err(not_found_response());
    };
    if session.viewing_as_dri.is_some() {
        return Err(not_found_response());
    }
    let Some(person_id) = session.person_id else {
        return Err(not_found_response());
    };
    let Some(project) = store::projects::find_by_code(surreal, code)
        .await
        .ok()
        .flatten()
    else {
        return Err(not_found_response());
    };
    if !crate::admin::is_lawyer_tier(Some(session)) {
        return Err(not_found_response());
    }
    // The JSON door's own admission, verbatim: a firm-side viewer of this
    // matter or the non-disclosing 404.
    let viewer = store::access::matter_viewer(surreal, session.person_id, session.role, project.id)
        .await
        .ok()
        .flatten();
    match viewer {
        Some(viewer) if viewer.is_firm_side() => {}
        _ => return Err(not_found_response()),
    }
    let Ok(pg) =
        crate::project_tasks_api::prepared_store(surreal, operation, project.id, Some(task_id))
            .await
    else {
        return Err(back_to_tasks(
            code,
            Some("The task store is unavailable right now — try again shortly."),
        ));
    };
    Ok(RowActionGate { person_id, pg })
}

/// An empty choice means "none"; anything else must parse as the UUID the
/// store will check against the participation ledger.
fn parse_assignee(raw: &str) -> Option<Uuid> {
    let raw = raw.trim();
    (!raw.is_empty())
        .then(|| Uuid::parse_str(raw).ok())
        .flatten()
}

/// A due date arrives as the date input's `YYYY-MM-DD`; anything else is a
/// refusal the handler turns into a flash.
fn parse_due_on(raw: &str) -> Option<NaiveDate> {
    let raw = raw.trim();
    (!raw.is_empty())
        .then(|| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
        .flatten()
}

/// The store's typed refusals, rendered as flash copy in the user's
/// vocabulary — never a variant name, never the wire's `TaskState` words.
fn flash_for(error: &store::tasks::ProjectTaskError) -> String {
    use store::tasks::ProjectTaskError;
    match error {
        ProjectTaskError::NotFound => "That task is not on this matter.".to_string(),
        ProjectTaskError::AssigneeNotParticipant => {
            "The person you chose is not a participant on this matter, so they cannot hold the task."
                .to_string()
        }
        ProjectTaskError::Transition { .. } => {
            "That task cannot move there — reload the tab and use the action it now offers."
                .to_string()
        }
        ProjectTaskError::Invalid(_) | ProjectTaskError::Database(_) | ProjectTaskError::Postgres(_) => {
            "The task store is unavailable right now — try again shortly.".to_string()
        }
    }
}
