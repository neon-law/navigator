//! Authenticated Relationship Log writes and observations attached to graph reads.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{bad_request, graph_store, participates_as_firm, ApiError, ApiState, LawyerSession};

use store::relationship_logs::scoped as graph_logs;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecordRelationshipObservation {
    request_id: Uuid,
    scope: graph_logs::Scope,
    firm_id: Option<Uuid>,
    project_id: Option<Uuid>,
    subject: graph_logs::Subject,
    subject_id: Uuid,
    action: String,
    detail: String,
}

pub(super) async fn record_relationship_observation(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Json(input): Json<RecordRelationshipObservation>,
) -> Result<Response, ApiError> {
    let actor_id = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    if lawyer.0.scope.is_some() || lawyer.0.source == crate::session::SessionSource::Ci {
        return Err(ApiError::Forbidden);
    }
    if input.action.is_empty()
        || input.action.len() > 48
        || !input
            .action
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        || input.detail.trim().is_empty()
        || input.detail.len() > 8192
    {
        return Ok(bad_request(
            "invalid_relationship_log",
            "Supply a lower snake_case type and a detail of up to 8 KiB.",
        ));
    }
    let firm_id = match (input.scope, input.project_id, input.firm_id) {
        (graph_logs::Scope::Project, Some(project_id), None) => {
            if !participates_as_firm(&state, &lawyer, project_id).await {
                return Err(ApiError::NotFound);
            }
            store::projects::find_by_id(&state.application_store, project_id)
                .await
                .map_err(|e| ApiError::Db(e.to_string()))?
                .and_then(|project| project.firm_id)
                .ok_or(ApiError::NotFound)?
        }
        (graph_logs::Scope::Attorney | graph_logs::Scope::Firm, None, Some(firm_id)) => firm_id,
        _ => {
            return Ok(bad_request(
                "invalid_relationship_scope",
                "Supply a Project for project scope or a Firm for attorney and firm scope.",
            ))
        }
    };
    if !has_relationship_log_membership(&state, &lawyer, firm_id).await {
        return Err(ApiError::NotFound);
    }
    let pg = match graph_store().await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let result = graph_logs::append(
        &pg,
        &graph_logs::Append {
            request_id: input.request_id,
            scope: input.scope,
            firm_id,
            project_id: input.project_id,
            actor_id,
            subject: input.subject,
            subject_id: input.subject_id,
            action: &input.action,
            detail: &input.detail,
        },
    )
    .await
    .map_err(|e| ApiError::Db(e.to_string()))?;
    match result {
        graph_logs::AppendResult::Recorded => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({"id": input.request_id})),
        )
            .into_response()),
        graph_logs::AppendResult::ScopeOrSubjectUnavailable => Err(ApiError::NotFound),
        graph_logs::AppendResult::RequestIdInUse => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "request_id_in_use"})),
        )
            .into_response()),
    }
}

#[derive(Serialize)]
struct GraphWithObservations<T> {
    #[serde(flatten)]
    view: T,
    relationship_logs: Vec<graph_logs::Observation>,
}

pub(super) async fn graph_with_observations(
    pg: &store::postgres::PostgresDb,
    scope: graph_logs::Scope,
    id: Uuid,
    viewer: Option<Uuid>,
    view: impl Serialize,
) -> Result<Response, ApiError> {
    let relationship_logs = if let Some(viewer) = viewer {
        graph_logs::for_graph(pg, scope, id, viewer)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?
    } else {
        Vec::new()
    };
    Ok(Json(GraphWithObservations {
        view,
        relationship_logs,
    })
    .into_response())
}

pub(super) async fn has_relationship_log_membership(
    state: &ApiState,
    lawyer: &LawyerSession,
    firm_id: Uuid,
) -> bool {
    store::firm_capability::FirmCapability::AccessRelationshipObservations
        .resolve(
            &state.application_store,
            lawyer.0.role,
            lawyer.0.person_id,
            firm_id,
        )
        .await
        .is_ok_and(store::firm_capability::FirmCapabilityDecision::is_allowed)
}

pub(super) async fn may_view_firm_graph(
    state: &ApiState,
    lawyer: &LawyerSession,
    firm_id: Uuid,
) -> bool {
    store::firm_capability::FirmCapability::ViewDirectory
        .resolve(
            &state.application_store,
            lawyer.0.role,
            lawyer.0.person_id,
            firm_id,
        )
        .await
        .is_ok_and(store::firm_capability::FirmCapabilityDecision::is_allowed)
}
