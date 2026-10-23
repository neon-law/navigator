//! Headless source import doors.
//!
//! `POST /app/api/projects/{id}/source-imports?operation_key=…` takes a raw
//! `application/zip` body, validates it with the shared headless source
//! archive rules, and records a durable import that the `SourceImport` DBOS
//! schedule applies to the Project's Firm repository on Forgejo. An invalid
//! archive changes nothing. `GET …/source-imports/{import_id}` reports where
//! an import stands. Lawyer tier on the matter; anything out of scope is 404.
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::project_source_imports::{self as imports, SourceImportError};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
pub(crate) struct ImportQuery {
    operation_key: String,
}

fn refused(error: &SourceImportError) -> Result<Response, ApiError> {
    let (status, code) = match error {
        SourceImportError::ProjectNotFound | SourceImportError::NotFound => {
            return Err(ApiError::NotFound)
        }
        SourceImportError::Archive(_) | SourceImportError::InvalidOperationKey => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_source_archive")
        }
        SourceImportError::NoFirm => (StatusCode::CONFLICT, "project_has_no_firm"),
        SourceImportError::OperationKeyConflict => (StatusCode::CONFLICT, "operation_key_conflict"),
        SourceImportError::ImportPending => (StatusCode::CONFLICT, "source_import_pending"),
        SourceImportError::AlreadyInitialized => {
            (StatusCode::CONFLICT, "source_already_initialized")
        }
        _ => {
            tracing::error!(%error, "source import request failed");
            (StatusCode::SERVICE_UNAVAILABLE, "source_import_unavailable")
        }
    };
    // The archive refusal names the rule and path, never content.
    Ok((
        status,
        Json(serde_json::json!({ "error": code, "detail": error.to_string() })),
    )
        .into_response())
}

fn unavailable(code: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

/// `POST /app/api/projects/{id}/source-imports`
pub(crate) async fn request(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project_id): Path<Uuid>,
    Query(query): Query<ImportQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let person = lawyer.0.person_id.ok_or(ApiError::NotFound)?;
    match cloud::ForgeBackend::from_lookup(|key| std::env::var(key).ok()) {
        Ok(cloud::ForgeBackend::Forgejo) => {}
        Ok(cloud::ForgeBackend::GitHub) => {
            return Ok((
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "source_import_requires_forgejo" })),
            )
                .into_response());
        }
        Err(_) => return Ok(unavailable("forge_backend_misconfigured")),
    }
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable("source_import_store_unavailable"));
    };
    match imports::request(
        &state.surreal,
        &pg,
        project_id,
        (person, lawyer.0.role),
        &query.operation_key,
        &body,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(import) => Ok((StatusCode::ACCEPTED, Json(import)).into_response()),
        Err(error) => refused(&error),
    }
}

/// `GET /app/api/projects/{id}/source-imports/{import_id}`
pub(crate) async fn status(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project_id, import_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    if !store::access::can_see_project_as_lawyer(
        &state.surreal,
        lawyer.0.person_id,
        lawyer.0.role,
        project_id,
    )
    .await
    .unwrap_or(false)
    {
        return Err(ApiError::NotFound);
    }
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable("source_import_store_unavailable"));
    };
    match imports::find(&pg, project_id, import_id).await {
        Ok(import) => Ok(Json(import).into_response()),
        Err(error) => refused(&error),
    }
}
