//! Native POST for `/app/admin/brands/{key}/edit`.
//!
//! The JSON command is `PATCH /app/api/brands/{key}`. This form twin
//! redirects back to the edit page so a native `<form>` stays post/redirect/get.

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::Extension;
use serde::Deserialize;

use crate::api::apply_brand_presentation;
use crate::session::SessionData;

/// `typeface` carries the chosen uploaded font family name directly (ENG-659:
/// the Admin edit page's select lists only this Firm's own uploads, never
/// the compiled catalog), or is absent when the Firm has none to choose from
/// yet — the page renders no control at all in that case. There is no
/// separate `font_family` field any more; choosing a family sets both of the
/// brand's font fields together, translated below.
#[derive(Debug, Deserialize)]
pub struct BrandPresentationForm {
    #[serde(default)]
    pub typeface: Option<String>,
    pub primary_color: String,
}

pub async fn post_brand_edit(
    State(surreal): State<store::surreal::SurrealDb>,
    Path(key): Path<String>,
    session: Option<Extension<SessionData>>,
    Form(form): Form<BrandPresentationForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    // An empty or absent selection means "leave the font unchanged" — a
    // Firm with no uploaded font yet has nothing valid to submit, and
    // `apply_brand_presentation` treats `None` that way for both fields
    // rather than erroring on a blank `typeface`.
    let chosen_family = form
        .typeface
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let typeface = chosen_family.map(|_| "uploaded");
    match apply_brand_presentation(
        &surreal,
        session.role,
        session.person_id,
        &key,
        typeface,
        &form.primary_color,
        chosen_family,
    )
    .await
    {
        Ok(_) => Redirect::to(&format!("/app/admin/brands/{key}/edit")).into_response(),
        Err(crate::api::BrandPresentationError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(crate::api::BrandPresentationError::UnknownChoice(message)) => {
            let mut query = String::new();
            crate::admin::push_query(&mut query, "error", &message);
            Redirect::to(&format!("/app/admin/brands/{key}/edit?{query}")).into_response()
        }
        Err(crate::api::BrandPresentationError::Internal(error)) => {
            tracing::error!(error = %error, "brand edit form failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn back_to_edit(key: &str, flash: Option<(&str, &str)>) -> Response {
    let mut query = String::new();
    if let Some((name, value)) = flash {
        crate::admin::push_query(&mut query, name, value);
    }
    if query.is_empty() {
        Redirect::to(&format!("/app/admin/brands/{key}/edit")).into_response()
    } else {
        Redirect::to(&format!("/app/admin/brands/{key}/edit?{query}")).into_response()
    }
}

/// A blank `website_url` clears the approval.
#[derive(Debug, Deserialize)]
pub struct BrandWebsiteForm {
    #[serde(default)]
    pub website_url: String,
}

/// `POST /app/admin/brands/{key}/website` — Owner approves or clears the
/// Brand's website URL. `store::brands::set_website_url` is the Owner gate;
/// any other caller gets the same 404 an unknown Brand does.
pub async fn post_brand_website(
    State(surreal): State<store::surreal::SurrealDb>,
    Path(key): Path<String>,
    session: Option<Extension<SessionData>>,
    Form(form): Form<BrandWebsiteForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if session.role != store::persons::Role::Owner {
        return StatusCode::NOT_FOUND.into_response();
    }
    let brand = match store::brands::find_by_key(&surreal, &key).await {
        Ok(Some(brand)) => brand,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(error = %error, "brand website lookup failed");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let website_url = Some(form.website_url.trim()).filter(|value| !value.is_empty());
    match store::brands::set_website_url(&surreal, session.role, brand.id, website_url).await {
        Ok(_) => back_to_edit(&key, Some(("website_saved", "true"))),
        Err(store::brands::BrandError::NotAuthorized) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => back_to_edit(&key, Some(("error", &error.user_message()))),
    }
}

/// `POST /app/admin/brands/{key}/attach` — the Brand's own Firm starts
/// wearing it. Owner, or that Firm's Admin; the Firm comes from the row.
pub async fn post_brand_attach(
    State(surreal): State<store::surreal::SurrealDb>,
    Path(key): Path<String>,
    session: Option<Extension<SessionData>>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match store::firms::attach_own_brand(&surreal, session.role, session.person_id, &key).await {
        Ok(_) => back_to_edit(&key, Some(("attached", "true"))),
        Err(store::firms::FirmError::NotAuthorized | store::firms::FirmError::UnknownBrand(_)) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(error) => back_to_edit(&key, Some(("error", &error.user_message()))),
    }
}
