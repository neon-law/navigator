//! Project Deadline doors.
//!
//! A write stores the due instant the caller named and returns that stored
//! row. Complete, cancel, and reopen change `status`; they keep the row.
//! Court days are never computed here.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use store::persons::Role;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDeadlineRequest {
    title: Option<String>,
    due_at: String,
    authority_kind: String,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    source: String,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    calendar_role: String,
    internal_lead_days: Option<i64>,
    client_lead_days: Option<i64>,
    replay_key: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(clippy::option_option)]
pub(crate) struct UpdateDeadlineRequest {
    /// An empty string clears the title.
    title: Option<String>,
    due_at: Option<String>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document_id: Option<Option<Uuid>>,
    owner_person_id: Option<Option<Uuid>>,
    calendar_role: Option<String>,
    internal_lead_days: Option<Option<i64>>,
    client_lead_days: Option<Option<i64>>,
}

async fn require_participant(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let allowed = store::access::can_see_project(&state.surreal, person_id, role, project_id)
        .await
        .map_err(ApiError::Db)?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

fn map_deadline(error: store::project_deadlines::DeadlineError) -> ApiError {
    match error {
        store::project_deadlines::DeadlineError::Invalid(message) => {
            ApiError::DeadlineInvalid(message)
        }
        store::project_deadlines::DeadlineError::NotFound => ApiError::NotFound,
        other => ApiError::Db(other.to_string()),
    }
}

/// `GET /app/api/projects/{id}/deadlines`
pub(crate) async fn list_deadlines(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let rows = store::project_deadlines::list(&state.surreal, id)
        .await
        .map_err(map_deadline)?;
    Ok(Json(rows).into_response())
}

/// `POST /app/api/projects/{id}/deadlines`
pub(crate) async fn create_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<CreateDeadlineRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::create(
        &state.surreal,
        &store::project_deadlines::NewDeadline {
            project_id: id,
            title: input.title,
            due_at: input.due_at,
            authority_kind: input.authority_kind,
            authority: input.authority,
            source: input.source,
            source_document_id: input.source_document_id,
            owner_person_id: input.owner_person_id,
            calendar_role: input.calendar_role,
            internal_lead_days: input.internal_lead_days,
            client_lead_days: input.client_lead_days,
            replay_key: input.replay_key,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok((StatusCode::CREATED, Json(row)).into_response())
}

/// `PATCH /app/api/projects/{id}/deadlines/{deadline_id}`
pub(crate) async fn update_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<UpdateDeadlineRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::update(
        &state.surreal,
        id,
        deadline_id,
        &store::project_deadlines::DeadlinePatch {
            title: input.title.map(Some),
            due_at: input.due_at,
            authority_kind: input.authority_kind,
            authority: input.authority,
            source: input.source,
            source_document_id: input.source_document_id,
            owner_person_id: input.owner_person_id,
            calendar_role: input.calendar_role,
            internal_lead_days: input.internal_lead_days,
            client_lead_days: input.client_lead_days,
        },
    )
    .await
    .map_err(map_deadline)?;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/complete`
pub(crate) async fn complete_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::complete(&state.surreal, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/cancel`
pub(crate) async fn cancel_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::cancel(&state.surreal, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/reopen`
pub(crate) async fn reopen_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::reopen(&state.surreal, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    Ok(Json(row).into_response())
}
