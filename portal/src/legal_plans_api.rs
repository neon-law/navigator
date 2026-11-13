//! A Firm's Legal Plans and the plan one Project is paid through. The plan
//! rows are Firm data in PostgreSQL (`docs/legal-plans.md`); a Project links
//! to one by its plan columns, the Plan Case Number first among them.
use super::{ApiError, ApiState, LawyerSession};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use store::firm_capability::FirmCapability;
use store::legal_plans::{self, LegalPlan, LegalPlanError, NewLegalPlan};
use store::projects::{PlanColumns, Project, ProjectCommandError};
use uuid::Uuid;

/// Refuse with 404 unless the session holds `capability` on `firm` — the
/// same answer as an unknown Firm, so a Firm boundary discloses nothing.
async fn authorize(
    state: &ApiState,
    session: &crate::SessionData,
    firm: Uuid,
    capability: FirmCapability,
) -> Result<(), ApiError> {
    let decision = capability
        .resolve(&state.surreal, session.role, session.person_id, firm)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    if !decision.is_allowed() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// Legal Plans are PostgreSQL-native; without PostgreSQL the request fails
/// with `503 legal_plans_unavailable` rather than answering from anywhere
/// else.
async fn plan_store() -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "legal_plans_unavailable" })),
        )
            .into_response()
    })
}

fn plan_error(error: &LegalPlanError) -> Response {
    let (status, code) = match error {
        LegalPlanError::Invalid(field) => {
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error": "legal_plan_invalid", "field": field})),
            )
                .into_response();
        }
        LegalPlanError::PlanNotInFirm => (StatusCode::UNPROCESSABLE_ENTITY, "legal_plan_refused"),
        LegalPlanError::NotFound => (StatusCode::NOT_FOUND, "legal_plan_not_found"),
        LegalPlanError::Exists => (StatusCode::CONFLICT, "legal_plan_exists"),
        LegalPlanError::Database(_) => (StatusCode::INTERNAL_SERVER_ERROR, "legal_plan_refused"),
    };
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

/// `GET /app/api/firms/{id}/legal-plans` — the Firm's plans, name order:
/// where each one is logged into, who to call, and its service levels. Any
/// member of the Firm may read them.
pub(super) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm, FirmCapability::ViewDirectory).await?;
    let pg = match plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(match legal_plans::list(&pg, firm).await {
        Ok(plans) => Json(plans).into_response(),
        Err(e) => plan_error(&e),
    })
}

/// `POST /app/api/firms/{id}/legal-plans` — add a plan to the Firm's
/// vocabulary. A name already held by the Firm is a 409.
pub(super) async fn create(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(input): Json<NewLegalPlan>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm, FirmCapability::ManageLegalPlans).await?;
    let pg = match plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(match legal_plans::create(&pg, firm, &input).await {
        Ok(plan) => (StatusCode::CREATED, Json(plan)).into_response(),
        Err(e) => plan_error(&e),
    })
}

/// The plan one Project is paid through: the Firm's plan row (portal,
/// contact, service levels) beside the matter's own plan columns.
#[derive(Debug, Serialize)]
struct ProjectPlan {
    project_id: Uuid,
    code: String,
    legal_plan: Option<LegalPlan>,
    plan_case_number: Option<String>,
    plan_member_id: Option<String>,
    benefit_code: Option<String>,
    plan_matter_type: Option<String>,
    plan_state: Option<String>,
    plan_assigned_on: Option<String>,
    plan_accepted_on: Option<String>,
    plan_closed_on: Option<String>,
}

/// The Project and its Firm, refused as 404 unless the session belongs to
/// that Firm. A Project with no Firm has no plan vocabulary to link into.
async fn firm_project(
    state: &ApiState,
    session: &crate::SessionData,
    project_id: Uuid,
) -> Result<(Project, Uuid), ApiError> {
    let project = store::projects::find_by_id(&state.surreal, project_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let firm = project.firm_id.ok_or(ApiError::NotFound)?;
    authorize(state, session, firm, FirmCapability::ViewDirectory).await?;
    Ok((project, firm))
}

/// The matter's plan columns beside its Firm plan row. PostgreSQL is read
/// only for a matter that links a plan.
async fn project_plan_view(firm: Uuid, project: Project) -> Result<ProjectPlan, Response> {
    let legal_plan = match project.legal_plan_id {
        Some(id) => legal_plans::find(&plan_store().await?, firm, id)
            .await
            .map_err(|e| plan_error(&e))?,
        None => None,
    };
    Ok(ProjectPlan {
        project_id: project.id,
        code: project.code,
        legal_plan,
        plan_case_number: project.plan_case_number,
        plan_member_id: project.plan_member_id,
        benefit_code: project.benefit_code,
        plan_matter_type: project.plan_matter_type,
        plan_state: project.plan_state,
        plan_assigned_on: project.plan_assigned_on,
        plan_accepted_on: project.plan_accepted_on,
        plan_closed_on: project.plan_closed_on,
    })
}

/// `GET /app/api/projects/{id}/legal-plan` — how this matter is paid: its
/// plan, Plan Case Number, and plan dates. `legal_plan` is null for a matter
/// no plan referred.
pub(super) async fn project(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let (project, firm) = firm_project(&state, &session, project_id).await?;
    Ok(match project_plan_view(firm, project).await {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused,
    })
}

/// The fields a `PATCH` may change. An absent field keeps its value; an
/// empty string clears it. `legal_plan` names one of the Firm's plans,
/// ignoring case.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectPlanPatch {
    legal_plan: Option<String>,
    plan_case_number: Option<String>,
    plan_member_id: Option<String>,
    benefit_code: Option<String>,
    plan_matter_type: Option<String>,
    plan_state: Option<String>,
    plan_assigned_on: Option<String>,
    plan_accepted_on: Option<String>,
    plan_closed_on: Option<String>,
}

/// Overlay one supplied value: absent keeps, empty clears, anything else
/// is trimmed and set.
fn overlay(current: &mut Option<String>, supplied: Option<String>) {
    if let Some(value) = supplied {
        let value = value.trim();
        *current = (!value.is_empty()).then(|| value.to_string());
    }
}

/// `PATCH /app/api/projects/{id}/legal-plan` — link the matter to one of
/// the Firm's plans and record its Plan Case Number and plan dates. The
/// fields not supplied keep their values; `store::projects::set_plan_columns`
/// validates the rest and refuses a plan of another Firm.
pub(super) async fn set_project(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(patch): Json<ProjectPlanPatch>,
) -> Result<Response, ApiError> {
    let (project, firm) = firm_project(&state, &session, project_id).await?;
    let pg = match plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    let mut columns = PlanColumns::of(&project);
    if let Some(name) = patch.legal_plan.as_deref().map(str::trim) {
        columns.legal_plan_id = if name.is_empty() {
            None
        } else {
            match legal_plans::find_by_name(&pg, firm, name).await {
                Ok(Some(plan)) => Some(plan.id),
                Ok(None) => {
                    return Err(ApiError::Project(ProjectCommandError::Invalid(
                        "legal_plan does not name one of the Firm's legal plans",
                    )))
                }
                Err(e) => return Ok(plan_error(&e)),
            }
        };
    }
    overlay(&mut columns.plan_case_number, patch.plan_case_number);
    overlay(&mut columns.plan_member_id, patch.plan_member_id);
    overlay(&mut columns.benefit_code, patch.benefit_code);
    overlay(&mut columns.plan_matter_type, patch.plan_matter_type);
    overlay(&mut columns.plan_state, patch.plan_state);
    overlay(&mut columns.plan_assigned_on, patch.plan_assigned_on);
    overlay(&mut columns.plan_accepted_on, patch.plan_accepted_on);
    overlay(&mut columns.plan_closed_on, patch.plan_closed_on);
    let updated = store::projects::set_plan_columns(&state.surreal, &pg, project_id, &columns)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(match project_plan_view(firm, updated).await {
        Ok(view) => Json(view).into_response(),
        Err(refused) => refused,
    })
}

#[cfg(test)]
mod tests {
    use super::overlay;

    #[test]
    fn an_absent_field_keeps_an_empty_one_clears_and_a_value_is_trimmed() {
        let mut kept = Some("4900000-001".to_string());
        overlay(&mut kept, None);
        assert_eq!(kept.as_deref(), Some("4900000-001"));
        overlay(&mut kept, Some("  ".into()));
        assert_eq!(kept, None);
        overlay(&mut kept, Some(" 412590 ".into()));
        assert_eq!(kept.as_deref(), Some("412590"));
    }
}
