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
use store::project_deadlines::{
    AuthorityKind, CalendarRole, Category, DeadlineIdentity, DueOnInput, Hearing,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateDeadlineRequest {
    title: Option<String>,
    description: Option<String>,
    due_at: Option<String>,
    due_on: Option<DueOnInput>,
    hearing: Option<Hearing>,
    authority_kind: AuthorityKind,
    #[serde(default)]
    authority: String,
    #[serde(default)]
    source: String,
    source_document_id: Option<Uuid>,
    owner_person_id: Option<Uuid>,
    calendar_role: CalendarRole,
    #[serde(
        default,
        deserialize_with = "store::project_deadlines::optional_category"
    )]
    category: Option<Category>,
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
    description: Option<String>,
    due_at: Option<String>,
    due_on: Option<DueOnInput>,
    hearing: Option<Hearing>,
    authority_kind: Option<String>,
    authority: Option<String>,
    source: Option<String>,
    source_document_id: Option<Option<Uuid>>,
    owner_person_id: Option<Option<Uuid>>,
    calendar_role: Option<String>,
    /// An empty string clears the category.
    category: Option<String>,
    internal_lead_days: Option<Option<i64>>,
    #[serde(default, deserialize_with = "present_client_lead")]
    client_lead_days: Option<Option<i64>>,
}

#[allow(clippy::option_option)]
fn present_client_lead<'de, D>(deserializer: D) -> Result<Option<Option<i64>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

async fn require_participant(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let allowed =
        store::access::can_see_project(&state.application_store, person_id, role, project_id)
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

/// The stored Deadline remains authoritative when Google needs a retry.
async fn mirror_after_write(state: &ApiState, project_id: Uuid) {
    match store::projects::find_by_id(&state.application_store, project_id).await {
        Ok(Some(project)) => {
            let result = crate::integrations_api::mirror_one(state, &project, false).await;
            if !matches!(
                result.outcome,
                "mirrored" | "skipped_closed" | "runtime_not_configured"
            ) {
                tracing::warn!(%project_id, outcome = result.outcome, "Deadline Calendar reconcile pending");
            }
        }
        Ok(None) | Err(_) => {
            tracing::warn!(%project_id, "Could not resolve Project for Deadline Calendar reconcile");
        }
    }
}

/// `GET /app/api/projects/{id}/deadlines`
pub(crate) async fn list_deadlines(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let rows = store::project_deadlines::list(&state.application_store, id)
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
    if model::project_deadlines::requires_court_review(
        &input.calendar_role.to_string(),
        &input.authority_kind.to_string(),
        input
            .category
            .map(|category| category.to_string())
            .as_deref(),
    ) {
        return Err(ApiError::DeadlineInvalid("Queue this court date with navigator project deadline review queue and confirm its evidence in Navigator.".into()));
    }
    let (due_at, due_on) = store::project_deadlines::resolve_due(
        &state.application_store,
        id,
        DeadlineIdentity::Replay(input.replay_key.as_deref()),
        input.due_at.as_deref(),
        input.due_on,
    )
    .await
    .map_err(map_deadline)?;
    let row = store::project_deadlines::create(
        &state.application_store,
        &store::project_deadlines::NewDeadline {
            project_id: id,
            title: input.title,
            description: input.description,
            due_on,
            hearing: input.hearing,
            due_at,
            authority_kind: input.authority_kind.to_string(),
            authority: input.authority,
            source: input.source,
            source_document_id: input.source_document_id,
            owner_person_id: input.owner_person_id,
            calendar_role: input.calendar_role.to_string(),
            category: input.category.map(|category| category.to_string()),
            internal_lead_days: input.internal_lead_days,
            client_lead_days: input.client_lead_days,
            replay_key: input.replay_key,
        },
    )
    .await
    .map_err(map_deadline)?;
    mirror_after_write(&state, id).await;
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
    let (due_at, due_on) = if input.due_at.is_some() || input.due_on.is_some() {
        let (at, on) = store::project_deadlines::resolve_due(
            &state.application_store,
            id,
            DeadlineIdentity::Id(deadline_id),
            input.due_at.as_deref(),
            input.due_on,
        )
        .await
        .map_err(map_deadline)?;
        (Some(at), on)
    } else {
        (None, None)
    };
    let row = store::project_deadlines::update(
        &state.application_store,
        id,
        deadline_id,
        &store::project_deadlines::DeadlinePatch {
            title: input.title.map(Some),
            description: input.description.map(Some),
            due_at,
            due_on,
            hearing: input.hearing,
            authority_kind: input.authority_kind,
            authority: input.authority,
            source: input.source,
            source_document_id: input.source_document_id,
            owner_person_id: input.owner_person_id,
            calendar_role: input.calendar_role,
            category: input
                .category
                .map(|value| if value.is_empty() { None } else { Some(value) }),
            internal_lead_days: input.internal_lead_days,
            client_lead_days: input.client_lead_days,
        },
    )
    .await
    .map_err(map_deadline)?;
    mirror_after_write(&state, id).await;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/complete`
pub(crate) async fn complete_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::complete(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    mirror_after_write(&state, id).await;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/cancel`
pub(crate) async fn cancel_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::cancel(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    mirror_after_write(&state, id).await;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/reopen`
pub(crate) async fn reopen_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_deadlines::reopen(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    mirror_after_write(&state, id).await;
    Ok(Json(row).into_response())
}
