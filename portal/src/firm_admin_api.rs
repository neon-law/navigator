//! JSON doors over Firm and Brand administration, so the CLI can rename a
//! Firm, set its Portal Theme Brand, move a Brand between Firms, detach a Brand, and delete an unworn
//! Brand or an empty Firm without the owner console.
//!
//! Every handler is a thin seam over one `store::firms` or `store::brands`
//! accessor, and that accessor is what authorizes: Owner for a move, Owner or
//! the Firm's own Admin for the rest. A refusal the caller can act on — a
//! Firm still owning Projects or Brands, a Brand still worn or named by a
//! Project — is a `409` carrying the store's own message. A scoped session
//! reaches none of these doors.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::brands::BrandError;
use store::firms::FirmError;
use uuid::Uuid;

use crate::api::{AdminSession, ApiError, ApiState};

fn conflict(message: &str) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": "conflict", "message": message })),
    )
        .into_response()
}

fn firm_refusal(error: &FirmError) -> Response {
    match error {
        FirmError::NotAuthorized
        | FirmError::NoSuchFirm(_)
        | FirmError::UnknownBrand(_)
        | FirmError::BrandHasNoFirm(_) => ApiError::NotFound.into_response(),
        FirmError::FirmOwnsProjects(_)
        | FirmError::FirmOwnsBrands(_)
        | FirmError::ThemeBrandNotOwned(_, _) => conflict(&error.user_message()),
        _ => {
            tracing::error!(%error, "api: firm administration failed");
            ApiError::Db(error.to_string()).into_response()
        }
    }
}

fn brand_refusal(error: &BrandError) -> Response {
    match error {
        BrandError::NotAuthorized | BrandError::NoSuchBrand(_) => {
            ApiError::NotFound.into_response()
        }
        BrandError::StillReferenced => conflict(&error.user_message()),
        _ => {
            tracing::error!(%error, "api: brand administration failed");
            ApiError::Db(error.to_string()).into_response()
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct EditFirm {
    #[serde(default)]
    name: Option<String>,
    /// Absent leaves the Portal Theme Brand alone; `null` clears it.
    #[expect(clippy::option_option, reason = "absent and null mean different edits")]
    #[serde(default, deserialize_with = "present")]
    portal_theme_brand_key: Option<Option<String>>,
}

/// A field that is present, even as `null`, is `Some`.
#[expect(clippy::option_option, reason = "absent and null mean different edits")]
fn present<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// `PATCH /app/api/firms/{id}` — rename a Firm, or set or clear its Portal
/// Theme Brand. A rename leaves its slug, and so its Forgejo organization,
/// unchanged.
pub(crate) async fn update(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
    Json(input): Json<EditFirm>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    let name = input.name.as_deref().map(str::trim);
    if name.is_some_and(str::is_empty) {
        return ApiError::MalformedBody("name must not be empty".into()).into_response();
    }
    if name.is_none() && input.portal_theme_brand_key.is_none() {
        return ApiError::MalformedBody("name or portal_theme_brand_key is required".into())
            .into_response();
    }
    let edit = store::firms::FirmEdit {
        name: name.map(str::to_string),
        portal_theme_brand_key: input.portal_theme_brand_key,
        ..store::firms::FirmEdit::default()
    };
    match store::firms::update(
        &state.application_store,
        session.role,
        session.person_id,
        firm_id,
        &edit,
    )
    .await
    {
        Ok(firm) => Json(serde_json::json!({
            "id": firm.id,
            "name": firm.name,
            "slug": firm.slug,
            "portal_theme_brand_key": firm.portal_theme_brand_key,
        }))
        .into_response(),
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/firms/{id}` — delete a Firm that owns no Projects and no
/// Brands.
pub(crate) async fn delete(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(firm_id): Path<Uuid>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::delete(
        &state.application_store,
        session.role,
        session.person_id,
        firm_id,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/firms/{id}/brands/{key}` — stop a Firm wearing a Brand.
pub(crate) async fn detach_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path((firm_id, key)): Path<(Uuid, String)>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::detach_brand(
        &state.application_store,
        session.role,
        session.person_id,
        firm_id,
        &key,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => firm_refusal(&error),
    }
}

#[derive(Deserialize)]
pub(crate) struct MoveBrand {
    firm_id: Uuid,
}

/// `PUT /app/api/brands/{key}/firm` — move a Brand to another Firm. Owner
/// only: a move changes whose legal entity the Brand's public pages name.
pub(crate) async fn move_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(key): Path<String>,
    Json(input): Json<MoveBrand>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    match store::firms::move_brand(
        &state.application_store,
        session.role,
        session.person_id,
        &key,
        input.firm_id,
    )
    .await
    {
        Ok(()) => {
            Json(serde_json::json!({ "brand": key, "firm_id": input.firm_id })).into_response()
        }
        Err(error) => firm_refusal(&error),
    }
}

/// `DELETE /app/api/brands/{key}` — delete a Brand no Firm wears and no
/// Project names.
pub(crate) async fn delete_brand(
    State(state): State<ApiState>,
    AdminSession(session): AdminSession,
    Path(key): Path<String>,
) -> Response {
    if session.scope.is_some() {
        return ApiError::Forbidden.into_response();
    }
    let brand = match store::brands::find_by_key(&state.application_store, &key).await {
        Ok(Some(brand)) => brand,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(error) => return brand_refusal(&error),
    };
    match store::brands::delete(
        &state.application_store,
        session.role,
        session.person_id,
        brand.id,
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => brand_refusal(&error),
    }
}

#[cfg(test)]
mod tests {}
