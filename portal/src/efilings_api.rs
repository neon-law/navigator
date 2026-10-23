//! E-Filing Transaction doors.
//!
//! A write records one status report for a transaction and returns the stored
//! row. The store refuses a filed date or conformed copy on a transaction that
//! is not accepted, so a pending filing never reads as filed.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordEfilingRequest {
    transaction_id: String,
    status: String,
    efsp: Option<String>,
    court: Option<String>,
    case_number: Option<String>,
    submitted_document_id: Option<Uuid>,
    filed_on: Option<String>,
    accepted_on: Option<String>,
    conformed_document_id: Option<Uuid>,
    needs_entered_order: Option<bool>,
    note: Option<String>,
}

async fn require_participant(
    state: &ApiState,
    session: &crate::SessionData,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let allowed =
        store::access::can_see_project(&state.surreal, session.person_id, session.role, project_id)
            .await
            .map_err(ApiError::Db)?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

fn map_efiling(error: store::efiling_transactions::EfilingError) -> ApiError {
    match error {
        store::efiling_transactions::EfilingError::Invalid(message) => {
            ApiError::EfilingInvalid(message)
        }
        other => ApiError::Db(other.to_string()),
    }
}

/// `GET /app/api/projects/{id}/efilings`
pub(crate) async fn list_efilings(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, &session, id).await?;
    let rows = store::efiling_transactions::list(&state.surreal, id)
        .await
        .map_err(map_efiling)?;
    Ok(Json(rows).into_response())
}

/// `POST /app/api/projects/{id}/efilings`
pub(crate) async fn record_efiling(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<RecordEfilingRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, &session, id).await?;
    let row = store::efiling_transactions::record(
        &state.surreal,
        &store::efiling_transactions::EfilingReport {
            project_id: id,
            transaction_id: input.transaction_id,
            status: input.status,
            efsp: input.efsp,
            court: input.court,
            case_number: input.case_number,
            submitted_document_id: input.submitted_document_id,
            filed_on: input.filed_on,
            accepted_on: input.accepted_on,
            conformed_document_id: input.conformed_document_id,
            needs_entered_order: input.needs_entered_order,
            note: input.note,
        },
    )
    .await
    .map_err(map_efiling)?;
    Ok(Json(row).into_response())
}
