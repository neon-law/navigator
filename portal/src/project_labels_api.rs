use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{
    postgres::PostgresDb,
    project_labels::{self, CatalogEntry, LabelError},
};
use uuid::Uuid;

use crate::api::{require_firm_administration, ApiError, ApiState, LawyerSession};
use crate::session::SessionData;

async fn firm_for_project(
    state: &ApiState,
    session: &SessionData,
    project_id: Uuid,
) -> Result<Uuid, ApiError> {
    if session.viewing_as_dri.is_some() {
        return Err(ApiError::NotFound);
    }
    let viewer = store::access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
    .await
    .map_err(ApiError::Db)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(ApiError::NotFound);
    }
    store::projects::find_by_id(&state.application_store, project_id)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .and_then(|project| project.firm_id)
        .ok_or(ApiError::NotFound)
}

fn store_error(error: LabelError) -> Response {
    let status = match error {
        LabelError::InvalidKey | LabelError::InvalidDisplay | LabelError::InvalidCount => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        LabelError::KeyConflict => StatusCode::CONFLICT,
        LabelError::Unavailable => StatusCode::NOT_FOUND,
        LabelError::Database(error) => return ApiError::Db(error.to_string()).into_response(),
    };
    (
        status,
        Json(serde_json::json!({ "error": error.to_string() })),
    )
        .into_response()
}

pub(crate) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<CatalogEntry>>, Response> {
    let firm_id = firm_for_project(&state, &session, project_id)
        .await
        .map_err(IntoResponse::into_response)?;
    let pg = PostgresDb::application()
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?;
    project_labels::catalog(&pg, firm_id, project_id)
        .await
        .map(Json)
        .map_err(store_error)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LabelCommand {
    Create {
        key: String,
        name: String,
        category: String,
    },
    Retire {
        key: String,
    },
    Restore {
        key: String,
    },
    Add {
        keys: Vec<String>,
    },
    Remove {
        keys: Vec<String>,
    },
}

pub(crate) async fn command(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(command): Json<LabelCommand>,
) -> Result<Json<Vec<CatalogEntry>>, Response> {
    let firm_id = firm_for_project(&state, &session, project_id)
        .await
        .map_err(IntoResponse::into_response)?;
    if matches!(
        &command,
        LabelCommand::Create { .. } | LabelCommand::Retire { .. } | LabelCommand::Restore { .. }
    ) {
        require_firm_administration(&state.application_store, &session, firm_id)
            .await
            .map_err(IntoResponse::into_response)?;
    }
    let pg = PostgresDb::application()
        .await
        .map_err(|error| ApiError::Db(error.to_string()).into_response())?;
    match command {
        LabelCommand::Create {
            key,
            name,
            category,
        } => project_labels::create(&pg, firm_id, &key, &name, &category).await,
        LabelCommand::Retire { key } => project_labels::set_retired(&pg, firm_id, &key, true).await,
        LabelCommand::Restore { key } => {
            project_labels::set_retired(&pg, firm_id, &key, false).await
        }
        LabelCommand::Add { keys } => {
            project_labels::set_assigned(&pg, firm_id, project_id, &keys, true).await
        }
        LabelCommand::Remove { keys } => {
            project_labels::set_assigned(&pg, firm_id, project_id, &keys, false).await
        }
    }
    .map_err(store_error)?;
    project_labels::catalog(&pg, firm_id, project_id)
        .await
        .map(Json)
        .map_err(store_error)
}
