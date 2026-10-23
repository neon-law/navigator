//! Legal-files-only handoff doors.
//!
//! A lawyer on the matter prepares a handoff for one participant
//! (`POST /app/api/projects/{id}/handoffs`); the recipient's own matter lens
//! decides which current legal files it holds, and every byte is verified
//! before the immutable manifest is recorded. The lawyer or the recipient reads
//! the manifest (`GET …/handoffs/{handoff_id}`) and downloads the bundle
//! (`GET …/handoffs/{handoff_id}/bundle`), rebuilt and re-verified from that
//! manifest. The recipient acknowledges the exact manifest they received
//! (`POST …/handoffs/{handoff_id}/acknowledge`). Anyone else gets 404.
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::project_handoffs::{self as handoffs, HandoffError};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, AuthedSession, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrepareHandoff {
    recipient_person_id: Uuid,
    /// Whether closing the Project requires this recipient's acknowledgment.
    #[serde(default)]
    required: bool,
}

fn refused(error: &HandoffError) -> Result<Response, ApiError> {
    let (status, code) = match error {
        HandoffError::ProjectNotFound | HandoffError::NotFound => return Err(ApiError::NotFound),
        HandoffError::RecipientNotParticipant => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "recipient_not_participant",
        ),
        HandoffError::NoFirm => (StatusCode::CONFLICT, "project_has_no_firm"),
        HandoffError::Missing(_) | HandoffError::Corrupt(_) => {
            (StatusCode::CONFLICT, "handoff_files_unverifiable")
        }
        HandoffError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "handoff_too_large"),
        _ => {
            tracing::error!(%error, "handoff failed");
            (StatusCode::SERVICE_UNAVAILABLE, "handoff_unavailable")
        }
    };
    Ok((
        status,
        Json(serde_json::json!({ "error": code, "detail": error.to_string() })),
    )
        .into_response())
}

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": "handoff_store_unavailable" })),
    )
        .into_response()
}

/// `POST /app/api/projects/{id}/handoffs`
pub(crate) async fn prepare(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<PrepareHandoff>,
) -> Result<Response, ApiError> {
    let person = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable());
    };
    let archive = cloud::archived_projects_from_env().await.ok();
    match handoffs::prepare(
        &state.surreal,
        &pg,
        &state.storage,
        archive.as_ref(),
        project_id,
        (person, lawyer.0.role),
        input.recipient_person_id,
        input.required,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(handoff) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "handoff": handoff, "manifest": handoff.manifest })),
        )
            .into_response()),
        Err(error) => refused(&error),
    }
}

/// The handoff, if the caller is a lawyer on the matter or its recipient.
async fn readable(
    state: &ApiState,
    session: &crate::session::SessionData,
    project_id: Uuid,
    handoff_id: Uuid,
) -> Result<Result<handoffs::Handoff, Response>, ApiError> {
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(Err(unavailable()));
    };
    let handoff = match handoffs::find(&pg, project_id, handoff_id).await {
        Ok(handoff) => handoff,
        Err(error) => return refused(&error).map(Err),
    };
    let lawyer = store::access::can_see_project_as_lawyer(
        &state.surreal,
        session.person_id,
        session.role,
        project_id,
    )
    .await
    .unwrap_or(false);
    if lawyer || session.person_id == Some(handoff.recipient_person_id) {
        Ok(Ok(handoff))
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /app/api/projects/{id}/handoffs/{handoff_id}`
pub(crate) async fn manifest(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project_id, handoff_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    match readable(&state, &session, project_id, handoff_id).await? {
        Ok(handoff) => Ok(Json(
            serde_json::json!({ "handoff": handoff, "manifest": handoff.manifest }),
        )
        .into_response()),
        Err(response) => Ok(response),
    }
}

/// `GET /app/api/projects/{id}/handoffs/{handoff_id}/bundle`
pub(crate) async fn bundle(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project_id, handoff_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let handoff = match readable(&state, &session, project_id, handoff_id).await? {
        Ok(handoff) => handoff,
        Err(response) => return Ok(response),
    };
    let archive = cloud::archived_projects_from_env().await.ok();
    match handoffs::bundle(&state.surreal, &state.storage, archive.as_ref(), &handoff).await {
        Ok(bytes) => Ok(Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/zip")
            .header(
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"handoff-{handoff_id}.zip\""),
            )
            .header("X-Manifest-SHA256", handoff.manifest_sha256.as_str())
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        Err(error) => refused(&error),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AcknowledgeHandoff {
    manifest_sha256: String,
}

/// `POST /app/api/projects/{id}/handoffs/{handoff_id}/acknowledge` — the
/// recipient confirms they received exactly the manifest this digest names.
pub(crate) async fn acknowledge(
    State(_state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path((project_id, handoff_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<AcknowledgeHandoff>,
) -> Result<Response, ApiError> {
    let person = session.person_id.ok_or(ApiError::NotFound)?;
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable());
    };
    match store::project_closeout::acknowledge(
        &pg,
        project_id,
        handoff_id,
        person,
        &input.manifest_sha256,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(acknowledgment) => Ok(Json(acknowledgment).into_response()),
        Err(
            store::project_closeout::AcknowledgmentError::NotFound
            | store::project_closeout::AcknowledgmentError::NotRecipient,
        ) => Err(ApiError::NotFound),
        Err(store::project_closeout::AcknowledgmentError::ManifestMismatch) => Ok((
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "manifest_mismatch" })),
        )
            .into_response()),
        Err(error) => {
            tracing::error!(%error, "handoff acknowledgment failed");
            Ok(unavailable())
        }
    }
}
