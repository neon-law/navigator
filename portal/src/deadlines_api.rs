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
    effort_minutes: Option<i32>,
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
    effort_minutes: Option<i32>,
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

pub(crate) async fn policy(
    state: &ApiState,
    project_id: Uuid,
) -> Result<model::project_deadlines::SchedulingPolicy, ApiError> {
    let project = store::projects::find_by_id(&state.application_store, project_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    store::project_deadlines::scheduling_policy(project.firm_id)
        .await
        .map_err(map_deadline)
}

fn scheduled_row(
    row: &store::project_deadlines::Deadline,
    policy: model::project_deadlines::SchedulingPolicy,
) -> serde_json::Value {
    let mut value = serde_json::json!(row);
    value["scheduling_policy"] = serde_json::json!(policy);
    value
}

async fn apply_calendar_schedule(
    state: &ApiState,
    row: &store::project_deadlines::Deadline,
    actor: Option<Uuid>,
) -> Option<serde_json::Value> {
    let person = row.owner_person_id.filter(|id| Some(*id) == actor)?;
    let project = store::projects::find_by_id(&state.application_store, row.project_id)
        .await
        .ok()??;
    let zone = store::projects::calendar_timezone(&state.application_store, &project)
        .await
        .ok()?;
    let result = crate::deadline_scheduling::sync(
        &state.application_store,
        state.integration_providers.as_ref(),
        person,
        zone,
        state.canonical_base_url.as_deref(),
    )
    .await;
    Some(match result {
        Ok(plan) => serde_json::json!({
            "booked": plan.booked,
            "work_blocks": plan.schedule.blocks.get(&row.id),
            "at_risk": plan.schedule.at_risk.contains_key(&row.id),
            "competing_deadlines": plan.schedule.at_risk.get(&row.id),
        }),
        Err(_) => serde_json::json!({ "availability": "unverified" }),
    })
}

async fn saved_deadline(
    state: &ApiState,
    row: &store::project_deadlines::Deadline,
    policy: model::project_deadlines::SchedulingPolicy,
    actor: Option<Uuid>,
) -> serde_json::Value {
    mirror_after_write(state, row.project_id).await;
    let schedule = apply_calendar_schedule(state, row, actor).await;
    let mut result = scheduled_row(row, policy);
    if row.status == store::project_deadlines::STATUS_OPEN {
        if let Some(schedule) = schedule {
            result["work_schedule"] = schedule;
        }
    }
    result
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
    let policy = policy(&state, id).await?;
    Ok(Json(
        rows.iter()
            .map(|row| scheduled_row(row, policy))
            .collect::<Vec<_>>(),
    )
    .into_response())
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
    let policy = policy(&state, id).await?;
    if input.category.is_some_and(Category::is_appearance) {
        if let Ok(start) = due_at.parse() {
            crate::deadline_scheduling::check_appearance(
                &state,
                &session,
                id,
                &[start],
                input.hearing.as_ref().map_or(60, Hearing::minutes),
                policy,
                input.replay_key.is_some(),
            )
            .await?;
        }
    }

    let row = store::project_deadlines::create(
        &state.application_store,
        &store::project_deadlines::NewDeadline {
            project_id: id,
            title: input.title,
            description: input.description,
            due_on,
            hearing: input.hearing,
            effort_minutes: input.effort_minutes,
            due_at,
            authority_kind: input.authority_kind.to_string(),
            authority: input.authority,
            source: input.source,
            source_document_id: input.source_document_id,
            owner_person_id: input.owner_person_id.or(session.person_id),
            calendar_role: input.calendar_role.to_string(),
            category: input.category.map(|category| category.to_string()),
            internal_lead_days: input.internal_lead_days,
            client_lead_days: input.client_lead_days,
            replay_key: input.replay_key,
        },
    )
    .await
    .map_err(map_deadline)?;
    let result = saved_deadline(&state, &row, policy, session.person_id).await;
    Ok((StatusCode::CREATED, Json(result)).into_response())
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
    let policy = policy(&state, id).await?;
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
            effort_minutes: input.effort_minutes,
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
    let result = saved_deadline(&state, &row, policy, session.person_id).await;
    Ok(Json(result).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/complete`
pub(crate) async fn complete_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let policy = policy(&state, id).await?;
    let row = store::project_deadlines::complete(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    let result = saved_deadline(&state, &row, policy, session.person_id).await;
    Ok(Json(result).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/cancel`
pub(crate) async fn cancel_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let policy = policy(&state, id).await?;
    let row = store::project_deadlines::cancel(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    let result = saved_deadline(&state, &row, policy, session.person_id).await;
    Ok(Json(result).into_response())
}

/// `POST /app/api/projects/{id}/deadlines/{deadline_id}/reopen`
pub(crate) async fn reopen_deadline(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, deadline_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let policy = policy(&state, id).await?;
    let row = store::project_deadlines::reopen(&state.application_store, id, deadline_id)
        .await
        .map_err(map_deadline)?;
    let result = saved_deadline(&state, &row, policy, session.person_id).await;
    Ok(Json(result).into_response())
}
