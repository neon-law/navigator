use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{
    firm_capability::FirmCapability,
    plan_claims::{self, Change, ClaimError},
};
use uuid::Uuid;

use super::{legal_plans_api, participates_as_firm, ApiError, ApiState, LawyerSession};

async fn scope(state: &ApiState, lawyer: &LawyerSession, project: Uuid) -> Result<Uuid, ApiError> {
    if !participates_as_firm(state, lawyer, project).await {
        return Err(ApiError::NotFound);
    }
    let (_, firm) = legal_plans_api::firm_project(state, &lawyer.0, project).await?;
    Ok(firm)
}

fn response<T: serde::Serialize>(result: Result<T, ClaimError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = match error {
                ClaimError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
                ClaimError::NotFound => StatusCode::NOT_FOUND,
                ClaimError::Duplicate => StatusCode::CONFLICT,
                ClaimError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

pub(super) async fn list(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project): Path<Uuid>,
) -> Result<Response, ApiError> {
    let firm = scope(&state, &lawyer, project).await?;
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(plan_claims::list(&pg, project, firm).await))
}

pub(super) async fn record(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project): Path<Uuid>,
    Json(change): Json<Change>,
) -> Result<Response, ApiError> {
    let firm = scope(&state, &lawyer, project).await?;
    let actor = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(
        plan_claims::record(&pg, project, firm, actor, change).await,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct WorklistQuery {
    #[serde(default)]
    totals_only: bool,
}

pub(super) async fn worklist(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Query(query): Query<WorklistQuery>,
) -> Result<Response, ApiError> {
    let projects = if query.totals_only {
        legal_plans_api::authorize(&state, &session, firm, FirmCapability::ViewPlanMetrics).await?;
        None
    } else {
        legal_plans_api::authorize(&state, &session, firm, FirmCapability::ViewDirectory).await?;
        let projects = store::access::visible_projects_as_lawyer(
            &state.application_store,
            session.person_id,
            session.role,
        )
        .await
        .map_err(ApiError::Db)?;
        Some(
            projects
                .into_iter()
                .filter(|project| project.firm_id == Some(firm))
                .map(|project| project.id)
                .collect::<Vec<_>>(),
        )
    };
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(
        plan_claims::worklist(&pg, firm, projects.as_deref()).await,
    ))
}

#[cfg(test)]
#[path = "plan_claims_api/tests.rs"]
mod tests;
