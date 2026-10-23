//! Project Recovery Archive doors: request a generation, list a Project's
//! generations, and re-verify a committed one.
//!
//! Admin tier, then Firm-scoped through
//! `FirmCapability::ManageRecoveryArchives`; anything out of scope is 404. A
//! request records durable intent and returns `202`: the `ProjectRecovery`
//! DBOS schedule copies, verifies, and commits it. Missing PostgreSQL or a
//! missing archived-projects lane is `503`, never a fallback.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::project_archives::{self as archives, ArchiveAuthority, ArchiveError};
use uuid::Uuid;

use crate::api::{AdminSession, ApiError, ApiState};

/// Body of `POST /app/api/projects/{id}/recovery-archives`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestArchive {
    /// Caller-chosen idempotency key: the same key returns the same generation.
    operation_key: String,
    /// The private receipt `navigator ops forgejo bundle archive` wrote; needed
    /// when the Project has a repository.
    #[serde(default)]
    source_backup: Option<archives::SourceBackupReference>,
}

fn unavailable(code: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

fn refused(error: &ArchiveError) -> Result<Response, ApiError> {
    let (status, code) = match error {
        ArchiveError::Forbidden
        | ArchiveError::ProjectNotFound
        | ArchiveError::GenerationNotFound
        | ArchiveError::NoFirm => return Err(ApiError::NotFound),
        ArchiveError::InvalidOperationKey
        | ArchiveError::SourceBackupRequired
        | ArchiveError::InvalidSourceBackup(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_archive_request")
        }
        ArchiveError::OperationKeyConflict => (StatusCode::CONFLICT, "operation_key_conflict"),
        ArchiveError::NotCommitted => (StatusCode::CONFLICT, "archive_not_committed"),
        ArchiveError::ManifestMismatch => (StatusCode::CONFLICT, "archive_manifest_mismatch"),
        _ => {
            tracing::error!(%error, "recovery archive operation failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "recovery_archive_unavailable",
            )
        }
    };
    Ok((status, Json(serde_json::json!({ "error": code }))).into_response())
}

/// Resolve the admin's Firm-scoped authority over `project_id`'s Firm.
async fn authority(
    state: &ApiState,
    admin: &AdminSession,
    project_id: Uuid,
) -> Result<ArchiveAuthority, ApiError> {
    let person = admin.0.person_id.ok_or(ApiError::NotFound)?;
    let project = store::projects::find_by_id(&state.surreal, project_id)
        .await
        .map_err(|_| ApiError::Db("the project could not be read".into()))?
        .ok_or(ApiError::NotFound)?;
    let firm_id = project.firm_id.ok_or(ApiError::NotFound)?;
    archives::authorize(&state.surreal, admin.0.role, person, firm_id)
        .await
        .map_err(|_| ApiError::NotFound)
}

/// `POST /app/api/projects/{id}/recovery-archives`
pub(crate) async fn request(
    State(state): State<ApiState>,
    admin: AdminSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<RequestArchive>,
) -> Result<Response, ApiError> {
    let authority = authority(&state, &admin, project_id).await?;
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable("recovery_archive_store_unavailable"));
    };
    let Ok(archive) = cloud::archived_projects_from_env().await else {
        return Ok(unavailable("archived_projects_lane_unconfigured"));
    };
    let stores = archives::ArchiveStores {
        surreal: &state.surreal,
        pg: &pg,
        documents: &state.storage,
        archive: &archive,
        source_backups: None,
    };
    match archives::request(
        &stores,
        &authority,
        project_id,
        &input.operation_key,
        input.source_backup,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(generation) => Ok((StatusCode::ACCEPTED, Json(generation)).into_response()),
        Err(error) => refused(&error),
    }
}

/// `GET /app/api/projects/{id}/recovery-archives`
pub(crate) async fn list(
    State(state): State<ApiState>,
    admin: AdminSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let authority = authority(&state, &admin, project_id).await?;
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable("recovery_archive_store_unavailable"));
    };
    match archives::for_project(&pg, &authority, project_id).await {
        Ok(generations) => {
            Ok(Json(serde_json::json!({ "generations": generations })).into_response())
        }
        Err(error) => refused(&error),
    }
}

/// `POST /app/api/projects/{id}/recovery-archives/{generation_id}/verify`
pub(crate) async fn verify(
    State(state): State<ApiState>,
    admin: AdminSession,
    Path((project_id, generation_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let authority = authority(&state, &admin, project_id).await?;
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return Ok(unavailable("recovery_archive_store_unavailable"));
    };
    let Ok(archive) = cloud::archived_projects_from_env().await else {
        return Ok(unavailable("archived_projects_lane_unconfigured"));
    };
    match archives::find(&pg, &authority, generation_id).await {
        Ok(generation) if generation.project_id == project_id => {}
        Ok(_) => return Err(ApiError::NotFound),
        Err(error) => return refused(&error),
    }
    match archives::verify(&pg, &archive, None, &authority, generation_id).await {
        Ok(report) => Ok(Json(report).into_response()),
        Err(error) => refused(&error),
    }
}
