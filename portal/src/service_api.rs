//! Service doors (LAW-124).
//!
//! A write records that one served document reached one recipient by one
//! method on one date, and returns the stored row. Reads name each row's
//! served document by its docket entry where one links it, else by slug, so
//! a caller composing a certificate or a document history can cite it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use store::persons::Role;
use store::project_service::ProjectService;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordServiceRequest {
    document_id: Option<Uuid>,
    /// The served document by its slug below `documents/`, instead of its id.
    slug: Option<String>,
    recipient: String,
    recipient_person_id: Option<Uuid>,
    recipient_entity_id: Option<Uuid>,
    counsel_for: Option<String>,
    method: String,
    served_on: String,
    address: String,
    declarant: String,
    declarant_person_id: Option<Uuid>,
    letter_id: Option<Uuid>,
    sent_email_id: Option<Uuid>,
}

/// One stored row plus how its served document is referenced: `D.I. 12`
/// where a docket entry links it, else the document's slug, else its id.
#[derive(Debug, Serialize)]
pub(crate) struct ServiceView {
    #[serde(flatten)]
    pub(crate) row: ProjectService,
    pub(crate) reference: String,
}

async fn require_participant(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let allowed = store::access::can_see_project(&state.surreal, person_id, role, project_id)
        .await
        .map_err(ApiError::Db)?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

fn map_service(error: store::project_service::ServiceError) -> ApiError {
    match error {
        store::project_service::ServiceError::Invalid(message) => ApiError::ServiceInvalid(message),
        store::project_service::ServiceError::NotFound => ApiError::NotFound,
        other => ApiError::Db(other.to_string()),
    }
}

/// How each served document is referenced, by asset id.
async fn references(
    state: &ApiState,
    project_id: Uuid,
    rows: &[ProjectService],
) -> Result<std::collections::HashMap<Uuid, String>, ApiError> {
    let mut out = std::collections::HashMap::new();
    let entries = store::cases::docket_for_project(&state.surreal, project_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    for entry in entries {
        if let Some(document) = entry.document_asset_id {
            out.entry(document)
                .or_insert_with(|| format!("D.I. {}", entry.entry_number));
        }
    }
    let unresolved: Vec<Uuid> = rows
        .iter()
        .map(|row| row.document_id)
        .filter(|id| !out.contains_key(id))
        .collect();
    for asset in store::assets::find_by_ids(&state.surreal, &unresolved)
        .await
        .map_err(ApiError::Asset)?
    {
        let reference = asset
            .slug
            .clone()
            .or(asset.filename.clone())
            .unwrap_or_else(|| asset.id.to_string());
        out.entry(asset.id).or_insert(reference);
    }
    Ok(out)
}

async fn views(
    state: &ApiState,
    project_id: Uuid,
    rows: Vec<ProjectService>,
) -> Result<Vec<ServiceView>, ApiError> {
    let references = references(state, project_id, &rows).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let reference = references
                .get(&row.document_id)
                .cloned()
                .unwrap_or_else(|| row.document_id.to_string());
            ServiceView { row, reference }
        })
        .collect())
}

/// `GET /app/api/projects/{id}/service`
pub(crate) async fn list_service(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let rows = store::project_service::list(&state.surreal, id)
        .await
        .map_err(map_service)?;
    Ok(Json(views(&state, id, rows).await?).into_response())
}

/// `GET /app/api/projects/{id}/service/{service_id}`
pub(crate) async fn show_service(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, service_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let row = store::project_service::find(&state.surreal, id, service_id)
        .await
        .map_err(map_service)?
        .ok_or(ApiError::NotFound)?;
    let mut rows = views(&state, id, vec![row]).await?;
    Ok(Json(rows.remove(0)).into_response())
}

/// `POST /app/api/projects/{id}/service`
pub(crate) async fn record_service(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<RecordServiceRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let document_id = match (input.document_id, input.slug.as_deref()) {
        (Some(document_id), _) => document_id,
        (None, Some(slug)) => {
            store::assets::current(&state.surreal, id, slug.trim(), store::assets::Lens::Lawyer)
                .await
                .map_err(ApiError::Asset)?
                .map(|asset| asset.id)
                .ok_or_else(|| {
                    ApiError::ServiceInvalid(format!("`{slug}` names no document on this Project"))
                })?
        }
        (None, None) => {
            return Err(ApiError::ServiceInvalid(
                "name the served document by document_id or slug".into(),
            ))
        }
    };
    let row = store::project_service::create(
        &state.surreal,
        &store::project_service::NewService {
            project_id: id,
            document_id,
            recipient: input.recipient,
            recipient_person_id: input.recipient_person_id,
            recipient_entity_id: input.recipient_entity_id,
            counsel_for: input.counsel_for,
            method: input.method,
            served_on: input.served_on,
            address: input.address,
            declarant: input.declarant,
            declarant_person_id: input.declarant_person_id,
            letter_id: input.letter_id,
            sent_email_id: input.sent_email_id,
        },
    )
    .await
    .map_err(map_service)?;
    let mut rows = views(&state, id, vec![row]).await?;
    Ok((StatusCode::CREATED, Json(rows.remove(0))).into_response())
}
