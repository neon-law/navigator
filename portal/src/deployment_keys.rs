//! Owner-only encrypted SOPS change requests. Never writes the deployment tree.
use crate::{admin::AdminState, session::SessionData};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{extract::State, Extension, Form, Router};
use std::collections::BTreeMap;

fn production() -> bool {
    store::deployment_secret_changes::accepts_changes_from(|key| std::env::var(key).ok())
}

fn owner(session: &SessionData) -> bool {
    session.role.is_owner()
}
fn redirect(result: &str) -> Response {
    let mut response = Redirect::to(&format!("/app/owner/keys?result={result}")).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
async fn save(
    State(state): State<AdminState>,
    Extension(session): Extension<SessionData>,
    Form(mut fields): Form<BTreeMap<String, String>>,
) -> Response {
    if !owner(&session) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !production() {
        return redirect("blocked");
    }
    let Some(actor_id) = session.person_id else {
        return StatusCode::NOT_FOUND.into_response();
    };
    for preset in store::deployment_secret_changes::PRESETS
        .iter()
        .filter(|preset| !preset.secret)
    {
        if fields
            .get(preset.key)
            .is_some_and(|value| std::env::var(preset.key).as_ref() == Ok(value))
        {
            fields.insert(preset.key.into(), String::new());
        }
    }
    match store::deployment_secret_changes::save(
        &state.surreal,
        state.runtime_kms.as_ref(),
        store::persons::Role::Owner,
        store::DeploymentEnvironment::Production,
        actor_id,
        &fields,
    )
    .await
    {
        Ok(0) => redirect("unchanged"),
        Ok(_) => redirect("saved"),
        Err(store::deployment_secret_changes::ChangeError::Invalid) => redirect("invalid"),
        Err(_) => redirect("blocked"),
    }
}
async fn test(Extension(session): Extension<SessionData>) -> Response {
    if !owner(&session) {
        return StatusCode::NOT_FOUND.into_response();
    }
    redirect("test")
}
async fn export(
    State(state): State<AdminState>,
    Extension(session): Extension<SessionData>,
) -> Response {
    if !owner(&session) || !production() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store::deployment_secret_changes::latest(
        &state.surreal,
        store::persons::Role::Owner,
        store::DeploymentEnvironment::Production,
    )
    .await
    {
        Ok(mut rows) => {
            rows.retain(|row| {
                store::deployment_secret_changes::exportable_at(row, chrono::Utc::now())
            });
            match serde_json::to_vec(&rows) {
                Ok(bytes) => (
                    [
                        (header::CONTENT_TYPE, "application/json"),
                        (
                            header::CONTENT_DISPOSITION,
                            "attachment; filename=production-social-changes.enc.json",
                        ),
                        (header::CACHE_CONTROL, "no-store"),
                    ],
                    bytes,
                )
                    .into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
/// Mutations inherit application authentication, policy and CSRF middleware.
pub fn routes() -> Router<AdminState> {
    Router::new()
        .route("/app/owner/keys", post(save))
        .route("/app/owner/keys/test", post(test))
        .route("/app/owner/keys/export", get(export))
}
