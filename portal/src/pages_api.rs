//! Matter Page door: `PUT /app/api/projects/{id}/pages`.
//!
//! `navigator project sync` sends the repository's `pages:` block here, and
//! the Project's stored set becomes exactly that block. The matter page reads
//! these rows, so rendering never reads git at request time. The set is
//! validated by the gate's own rules before anything is written, so a page
//! the gate would refuse never reaches the renderer.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutPagesRequest {
    pages: Vec<rules::pages::PageEntry>,
}

/// The stored set, in declared order.
#[derive(Debug, Serialize)]
pub(crate) struct PagesView {
    pages: Vec<rules::pages::PageEntry>,
    synced_at: Option<String>,
}

/// `PUT /app/api/projects/{id}/pages`
pub(crate) async fn put_pages(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<PutPagesRequest>,
) -> Result<Response, ApiError> {
    let allowed =
        store::access::can_see_project(&state.surreal, session.person_id, session.role, id)
            .await
            .map_err(ApiError::Db)?;
    if !allowed {
        return Err(ApiError::NotFound);
    }
    let stored = store::matter_pages::replace_for_project(&state.surreal, id, &input.pages)
        .await
        .map_err(|error| match error {
            store::matter_pages::MatterPageError::Invalid(findings) => {
                ApiError::PagesInvalid(findings.join("; "))
            }
            other @ store::matter_pages::MatterPageError::Db(_) => ApiError::Db(other.to_string()),
        })?;
    Ok(Json(PagesView {
        synced_at: stored.first().map(|page| page.synced_at.clone()),
        pages: stored.into_iter().map(|page| page.entry).collect(),
    })
    .into_response())
}
