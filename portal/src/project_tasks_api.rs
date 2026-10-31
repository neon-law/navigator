//! Assignable matter work: reads and commands over the PostgreSQL task store.
//!
//! The Tasks tab's write surface — create, assign, transition — beside the
//! project-costs doors. Route admission is lawyer tier (the `LawyerSession`
//! extractor plus an embedded-Rego `route_allow` rule); the matter-scope
//! question is asked once through [`store::access::matter_viewer`], so a
//! caller with no firm-side participation row — a client, a clerk, an
//! unassigned lawyer, Owner and Admin included — gets the non-disclosing 404,
//! never a refusal.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{postgres_mirror, tasks};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

fn refused(status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({"error": "project_task_command_refused"})),
    )
        .into_response()
}

fn error(
    error: &tasks::ProjectTaskError,
    operation: &'static str,
    project_id: Uuid,
    task_id: Option<Uuid>,
) -> Response {
    let status = match error {
        tasks::ProjectTaskError::Invalid(_)
        | tasks::ProjectTaskError::Transition { .. }
        | tasks::ProjectTaskError::AssigneeNotParticipant => StatusCode::UNPROCESSABLE_ENTITY,
        tasks::ProjectTaskError::NotFound => StatusCode::NOT_FOUND,
        tasks::ProjectTaskError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        tasks::ProjectTaskError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "project_task_operation_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            task_id = ?task_id,
            store_error = %error
        );
    }
    refused(status)
}

/// The matter surface's own question, asked once: `store::access` requires the
/// participation row of every tier (ENG-81), and the Tasks tab is a firm-side
/// surface, so only the Lawyer and LawyerDri viewers pass. Everyone else —
/// including a client-side participant and an unassigned Owner — collapses to
/// the non-disclosing 404.
async fn ensure_firm_side(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: store::persons::Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let viewer = store::access::matter_viewer(&state.surreal, person_id, role, project_id)
        .await
        .map_err(ApiError::Db)?;
    match viewer {
        Some(viewer) if viewer.is_firm_side() => Ok(()),
        _ => Err(ApiError::NotFound),
    }
}

async fn postgres(
    operation: &'static str,
    project_id: Uuid,
    task_id: Option<Uuid>,
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "project_task_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id,
            task_id = ?task_id
        );
        refused(StatusCode::SERVICE_UNAVAILABLE)
    })
}

/// Open the task store with its dependency rows mirrored: `store::tasks`
/// checks the parent Project row and the assignee's participation in
/// PostgreSQL, so a Project or participant added before the mirror was
/// enabled would otherwise be wrongly refused there. Mirroring the
/// participation rows also carries their Persons (the participation image
/// ensures the Person first), which is every caller and assignee this door
/// can reach — both are participation-scoped.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
    task_id: Option<Uuid>,
) -> Result<store::postgres::PostgresDb, Response> {
    let pg = postgres(operation, project_id, task_id).await?;
    let mirrored = async {
        postgres_mirror::mirror(
            &state.surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await?;
        let participations =
            store::projects::participations_for_project(&state.surreal, project_id)
                .await
                .map_err(|_| postgres_mirror::MirrorError::Source)?;
        for role in participations {
            postgres_mirror::mirror(
                &state.surreal,
                &pg,
                &postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await?;
        }
        Ok::<_, postgres_mirror::MirrorError>(())
    }
    .await;
    if let Err(store_error) = mirrored {
        tracing::warn!(
            event = "project_task_mirror_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            task_id = ?task_id,
            store_error = %store_error
        );
        return Err(refused(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok(pg)
}

/// `GET /app/api/projects/{id}/tasks` — the matter's Tasks tab rows.
pub(crate) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("list", project_id, None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match tasks::list_for_project(&pg, project_id).await {
        Ok(rows) => Ok(Json(serde_json::json!({"tasks": rows})).into_response()),
        Err(e) => Ok(error(&e, "list", project_id, None)),
    }
}

/// `POST /app/api/projects/{id}/tasks` — open a Task in the `todo` state.
pub(crate) async fn create(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<tasks::NewProjectTask>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "create", project_id, None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match tasks::create(&pg, project_id, actor_id, &input).await {
        Ok(row) => Ok((StatusCode::CREATED, Json(row)).into_response()),
        Err(e) => Ok(error(&e, "create", project_id, None)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignInput {
    assignee_id: Option<Uuid>,
}

/// `POST /app/api/projects/{id}/tasks/{task_id}/assign` — set or clear the
/// assignee. `None` clears; repeating the current assignee is a no-op.
pub(crate) async fn assign(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, task_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<AssignInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "assign", project_id, Some(task_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let task = match tasks::find_by_id(&pg, task_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "assign", project_id, Some(task_id))),
    };
    if task.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    Ok(
        match tasks::assign(&pg, task_id, actor_id, input.assignee_id).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => error(&e, "assign", project_id, Some(task_id)),
        },
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TransitionInput {
    state: String,
}

/// The stored word is the wire word: the task store's vocabulary is
/// `todo` → `in_progress` → `done`, and the API never invents a second one.
fn parse_state(raw: &str) -> Option<tasks::TaskState> {
    match raw {
        "todo" => Some(tasks::TaskState::Todo),
        "in_progress" => Some(tasks::TaskState::InProgress),
        "done" => Some(tasks::TaskState::Done),
        _ => None,
    }
}

/// `POST /app/api/projects/{id}/tasks/{task_id}/transition` — move a Task one
/// step forward. Done is a transition, never a delete.
pub(crate) async fn transition(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, task_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<TransitionInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "transition", project_id, Some(task_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let task = match tasks::find_by_id(&pg, task_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "transition", project_id, Some(task_id))),
    };
    if task.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    let Some(next) = parse_state(&input.state) else {
        return Ok(refused(StatusCode::UNPROCESSABLE_ENTITY));
    };
    Ok(
        match tasks::transition(&pg, task_id, actor_id, next).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => error(&e, "transition", project_id, Some(task_id)),
        },
    )
}
