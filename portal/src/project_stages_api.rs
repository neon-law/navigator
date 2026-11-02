//! The matter's Case Stage read and command surface: the dashboard's stage
//! chip and its history, beside the project-costs and project-task doors.
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule); the matter-scope question is asked once
//! through [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row — a client, a clerk, an unassigned lawyer, Owner and
//! Admin included — gets the non-disclosing 404, never a refusal.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{case_stages, postgres_mirror};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

fn refused(status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({"error": "case_stage_command_refused"})),
    )
        .into_response()
}

fn error(
    error: &case_stages::CaseStageError,
    operation: &'static str,
    project_id: Uuid,
) -> Response {
    let status = match error {
        case_stages::CaseStageError::Invalid(_)
        | case_stages::CaseStageError::NotInFirmVocabulary
        | case_stages::CaseStageError::ProjectHasNoFirm => StatusCode::UNPROCESSABLE_ENTITY,
        case_stages::CaseStageError::NotFound => StatusCode::NOT_FOUND,
        case_stages::CaseStageError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        case_stages::CaseStageError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "case_stage_operation_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %error
        );
    }
    refused(status)
}

/// The matter surface's own question, asked once: `store::access` requires the
/// participation row of every tier (ENG-81), and the stage surface is a
/// firm-side surface, so only the Lawyer and LawyerDri viewers pass. Everyone
/// else — including a client-side participant and an unassigned Owner —
/// collapses to the non-disclosing 404.
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
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "case_stage_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id
        );
        refused(StatusCode::SERVICE_UNAVAILABLE)
    })
}

/// Open the stage store with its dependency rows mirrored: `set_stage` reads
/// the parent Project row (with its `firm_id`) in PostgreSQL, and the history
/// insert references the actor's Person row — so a Project or participant
/// added before the mirror was enabled would otherwise be wrongly refused
/// there. Mirroring the participation rows also carries their Persons, which
/// covers every actor this door can admit.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    let pg = postgres(operation, project_id).await?;
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
            event = "case_stage_mirror_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %store_error
        );
        return Err(refused(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok(pg)
}

/// `GET /app/api/projects/{id}/stages` — the matter's stage read: the Firm's
/// vocabulary, the stage the matter currently holds (`None` until the Firm
/// places it), and the audited history, oldest first.
pub(crate) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let Some(project) = store::projects::find_by_id(&state.surreal, project_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
    else {
        return Err(ApiError::NotFound);
    };
    let pg = match postgres("list", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let read = async {
        let definitions = match project.firm_id {
            Some(firm_id) => case_stages::list_definitions(&pg, firm_id).await?,
            None => Vec::new(),
        };
        let current = case_stages::current_stage(&pg, project_id).await?;
        let history = case_stages::history(&pg, project_id).await?;
        Ok::<_, case_stages::CaseStageError>((definitions, current, history))
    }
    .await;
    match read {
        Ok((definitions, current, history)) => Ok(Json(serde_json::json!({
            "definitions": definitions,
            "current": current,
            "history": history,
        }))
        .into_response()),
        Err(e) => Ok(error(&e, "list", project_id)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetStageInput {
    stage_id: Uuid,
    note: Option<String>,
}

/// `POST /app/api/projects/{id}/stages/set` — move the matter to a stage from
/// the Firm's own vocabulary, appending the audited history row. Setting the
/// stage the matter already holds is a no-op that records no event.
pub(crate) async fn set(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<SetStageInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "set", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match case_stages::set_stage(&pg, project_id, actor_id, input.stage_id, input.note).await {
        Ok(event) => Ok(Json(event).into_response()),
        Err(e) => Ok(error(&e, "set", project_id)),
    }
}
