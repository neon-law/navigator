//! Matter Caption door: `PUT /app/api/projects/{id}/matter`.
//!
//! `navigator project sync` sends the repository's `matter:` block here, and
//! the Project's stored Matter Caption becomes exactly that block; `null` or
//! an empty block removes it. The matter page's caption and party roster read
//! the row, so rendering never reads git at request time.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutMatterRequest {
    matter: Option<word::matter::Matter>,
}

/// The stored block, or `null` when the Project has none.
#[derive(Debug, Serialize)]
pub(crate) struct MatterView {
    matter: Option<word::matter::Matter>,
    synced_at: Option<String>,
}

/// `PUT /app/api/projects/{id}/matter`
pub(crate) async fn put_matter(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<PutMatterRequest>,
) -> Result<Response, ApiError> {
    let allowed =
        store::access::can_see_project(&state.surreal, session.person_id, session.role, id)
            .await
            .map_err(ApiError::Db)?;
    if !allowed {
        return Err(ApiError::NotFound);
    }
    let stored =
        store::matter_captions::replace_for_project(&state.surreal, id, input.matter.as_ref())
            .await
            .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok(Json(MatterView {
        synced_at: stored.as_ref().map(|caption| caption.synced_at.clone()),
        matter: stored.map(|caption| caption.matter),
    })
    .into_response())
}
