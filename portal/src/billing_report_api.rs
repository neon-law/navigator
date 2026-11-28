use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use model::billing_report::{BillingAction, BillingReport};
use store::{
    access::{self, MatterViewer},
    billing_report::{self, ProjectScope},
    firm_capability::FirmCapability,
    postgres::PostgresDb,
    projects::{self, Project},
};
use uuid::Uuid;

use super::{legal_plans_api, ApiError, ApiState, LawyerSession};

pub(super) async fn personal(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
) -> Result<Response, ApiError> {
    let person = session.person_id.ok_or(ApiError::Forbidden)?;
    let projects =
        access::visible_projects_as_lawyer(&state.application_store, Some(person), session.role)
            .await
            .map_err(ApiError::Db)?;
    let pg = match postgres().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    let mut report = BillingReport::default();
    for project in projects {
        let view = access::matter_viewer(
            &state.application_store,
            Some(person),
            session.role,
            project.id,
        )
        .await
        .map_err(ApiError::Db)?;
        if let Some(view) = view.filter(|view| view.is_firm_side()) {
            add(&mut report, &state, &pg, &session, &project, view, false).await?;
        }
    }
    Ok(result(report))
}

pub(super) async fn project(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let view = access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        id,
    )
    .await
    .map_err(ApiError::Db)?
    .filter(|view| view.is_firm_side())
    .ok_or(ApiError::NotFound)?;
    let project = projects::find_by_id(&state.application_store, id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .ok_or(ApiError::NotFound)?;
    let pg = match postgres().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    let mut report = BillingReport::default();
    add(&mut report, &state, &pg, &session, &project, view, true).await?;
    Ok(result(report))
}

async fn postgres() -> Result<PostgresDb, Response> {
    PostgresDb::application().await.map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "billing_unavailable"})),
        )
            .into_response()
    })
}

async fn add(
    report: &mut BillingReport,
    state: &ApiState,
    pg: &PostgresDb,
    session: &crate::SessionData,
    project: &Project,
    view: MatterViewer,
    project_view: bool,
) -> Result<(), ApiError> {
    let attorney = session.person_id.ok_or(ApiError::Forbidden)?;
    let responsible = view == MatterViewer::LawyerDri;
    let claims_firm = if let Some(firm) = project.firm_id {
        match legal_plans_api::authorize(state, session, firm, FirmCapability::ViewDirectory).await
        {
            Ok(()) => Some(firm),
            Err(ApiError::NotFound) => None,
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    billing_report::add_project(
        report,
        pg,
        project,
        attorney,
        Utc::now().date_naive(),
        ProjectScope {
            responsible,
            project_view,
            claims_firm,
        },
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))
}

fn result(mut report: BillingReport) -> Response {
    report
        .needs_my_action
        .sort_by(|left, right| action_key(left).cmp(&action_key(right)));
    let time = report.project_time.as_mut().unwrap_or(&mut report.my_time);
    time.sort_by(|a, b| {
        b.performed_on
            .cmp(&a.performed_on)
            .then_with(|| a.project.cmp(&b.project))
            .then(a.id.cmp(&b.id))
    });
    Json(report).into_response()
}

fn action_key(action: &BillingAction) -> (Option<chrono::NaiveDate>, &str, Option<Uuid>) {
    match action {
        BillingAction::Claim(claim) => (Some(claim.performed_on), &claim.project, Some(claim.id)),
        BillingAction::PendingAnnotation {
            project,
            annotation_id,
            performed_on,
            ..
        } => (Some(*performed_on), project, Some(*annotation_id)),
        BillingAction::PendingProposals { project, .. }
        | BillingAction::UnallocatedWork { project, .. } => (None, project, None),
    }
}
