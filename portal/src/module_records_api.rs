//! Read doors for the rows behind the `estate` and `cap_table` Modules:
//! `GET /app/api/projects/{id}/fiduciary-roles`, `.../estate-assets`, and
//! `.../cap-table-holdings`.
//!
//! The policy admits any authenticated session; the handler resolves the
//! caller's lens on the Project (`store::access::matter_viewer`):
//!
//! - The firm side reads every row on a matter it participates in.
//! - The client side reads every row on their own matter while the owning
//!   Module is enabled, and an empty list otherwise, so a disabled Module
//!   reads exactly like one with no rows.
//! - A Clerk, a non-participant, and an unknown Project get the same 404.
//!
//! The rows are written by seed import (`navigator site import`), never
//! through these doors.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use store::access::MatterViewer;
use store::project_modules::Module;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, AuthedSession};

/// Whether the caller reads the rows owned by `module` on `project_id`:
/// `Ok(true)` for every row, `Ok(false)` for an empty list, 404 when the
/// caller has no lens on the matter that shows estate or cap-table records.
async fn reads_rows(
    state: &ApiState,
    authed: &AuthedSession,
    project_id: Uuid,
    module: Module,
) -> Result<bool, ApiError> {
    let viewer = store::access::matter_viewer(
        &state.surreal,
        authed.0.person_id,
        authed.0.role,
        project_id,
    )
    .await
    .map_err(ApiError::Db)?;
    match viewer {
        Some(MatterViewer::Lawyer | MatterViewer::LawyerDri) => Ok(true),
        Some(MatterViewer::Client | MatterViewer::ClientDri) => {
            store::project_modules::is_enabled(&state.surreal, project_id, module)
                .await
                .map_err(|error| ApiError::Db(error.to_string()))
        }
        Some(MatterViewer::Clerk) | None => Err(ApiError::NotFound),
    }
}

fn rows<T: Serialize>(rows: Vec<T>) -> Response {
    Json(rows).into_response()
}

/// `GET /app/api/projects/{id}/fiduciary-roles`
pub(crate) async fn list_fiduciary_roles(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !reads_rows(&state, &authed, id, Module::Estate).await? {
        return Ok(rows(Vec::<store::fiduciary_roles::FiduciaryRole>::new()));
    }
    store::fiduciary_roles::list(&state.surreal, id)
        .await
        .map(rows)
        .map_err(|error| ApiError::Db(error.to_string()))
}

/// `GET /app/api/projects/{id}/estate-assets`
pub(crate) async fn list_estate_assets(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !reads_rows(&state, &authed, id, Module::Estate).await? {
        return Ok(rows(Vec::<store::estate_assets::EstateAsset>::new()));
    }
    store::estate_assets::list(&state.surreal, id)
        .await
        .map(rows)
        .map_err(|error| ApiError::Db(error.to_string()))
}

/// `GET /app/api/projects/{id}/cap-table-holdings`
pub(crate) async fn list_cap_table_holdings(
    State(state): State<ApiState>,
    authed: AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if !reads_rows(&state, &authed, id, Module::CapTable).await? {
        return Ok(rows(
            Vec::<store::cap_table_holdings::CapTableHolding>::new(),
        ));
    }
    store::cap_table_holdings::list(&state.surreal, id)
        .await
        .map(rows)
        .map_err(|error| ApiError::Db(error.to_string()))
}
