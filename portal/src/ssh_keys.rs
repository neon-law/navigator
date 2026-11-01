//! A Firm user's SSH public keys (ENG-1219): the `/app/api/ssh-keys` doors
//! `navigator site person ssh-key` calls, and the native forms on the
//! self-service `/app/profile` page.
//!
//! A caller registers and lists only their own keys; the Person is always the
//! signed session's, never one named by the request. Revocation is the one
//! door that may reach another Person's key: a Firm administrator who may
//! manage one of the key owner's memberships, or an Owner
//! (`store::ssh_public_keys::may_revoke`). Anyone else gets the same 404 as a
//! missing key. Every write is followed by a reconciliation of the key
//! owner's Forgejo account; a reconciliation that cannot run is reported in
//! the reply, never a failure of the write, and the operator sweep
//! (`navigator ops forgejo ssh-keys sync`) converges it later. Scoped (CI)
//! sessions are refused.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::post;
use axum::{Extension, Form, Json, Router};
use serde::Deserialize;
use store::postgres::PostgresDb;
use store::ssh_public_keys::{self as keys, SshKeyError, SshPublicKey};
use uuid::Uuid;

use crate::admin::AdminState;
use crate::api::{ApiError, ApiState, AuthedSession};
use crate::session::SessionData;

/// The profile page's add form posts here.
pub const PROFILE_SSH_KEYS_PATH: &str = "/app/profile/ssh-keys";

fn json_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(serde_json::json!({ "error": code, "message": message })),
    )
        .into_response()
}

/// The wire status and stable code for a refused key operation.
fn refusal(error: &SshKeyError) -> (StatusCode, &'static str) {
    match error {
        SshKeyError::PrivateKey => (StatusCode::UNPROCESSABLE_ENTITY, "private_key"),
        SshKeyError::UnsupportedAlgorithm => {
            (StatusCode::UNPROCESSABLE_ENTITY, "unsupported_algorithm")
        }
        SshKeyError::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_key"),
        SshKeyError::InvalidLabel => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_label"),
        SshKeyError::Duplicate => (StatusCode::CONFLICT, "duplicate_key"),
        SshKeyError::NotFirmMember => (StatusCode::FORBIDDEN, "not_firm_member"),
        SshKeyError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        SshKeyError::PersonNotFound => (StatusCode::CONFLICT, "person_not_available"),
        SshKeyError::Postgres(_) => (StatusCode::SERVICE_UNAVAILABLE, "store_unavailable"),
        SshKeyError::Sync(_)
        | SshKeyError::Database(_)
        | SshKeyError::Firm(_)
        | SshKeyError::Person(_) => {
            tracing::error!(%error, "SSH key operation failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "ssh_key_failed")
        }
    }
}

fn refused(error: &SshKeyError) -> Response {
    let (status, code) = refusal(error);
    json_error(status, code, &error.to_string())
}

fn unavailable() -> Response {
    json_error(
        StatusCode::SERVICE_UNAVAILABLE,
        "store_unavailable",
        "PostgreSQL is unavailable",
    )
}

fn scoped() -> Response {
    json_error(
        StatusCode::FORBIDDEN,
        "scope_violation",
        "a scoped session cannot manage SSH keys",
    )
}

/// Reconcile `person_id`'s Forgejo account and describe the outcome. A
/// failure is reported, not raised: the key change already stands.
async fn sync(
    surreal: &store::surreal::SurrealDb,
    pg: &PostgresDb,
    person_id: Uuid,
) -> serde_json::Value {
    match keys::sync_person_from_env(surreal, pg, person_id).await {
        Ok(outcome) => serde_json::to_value(outcome)
            .unwrap_or_else(|_| serde_json::json!({ "status": "failed" })),
        Err(error) => {
            tracing::warn!(%error, person_id = %person_id, "Forgejo SSH key sync failed");
            serde_json::json!({ "status": "failed", "error": error.to_string() })
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    /// Another Person whose keys the caller may revoke. Omitted: the
    /// caller's own.
    person: Option<Uuid>,
}

/// `GET /app/api/ssh-keys` — the caller's keys, or with `?person=` another
/// Person's keys the caller may revoke.
pub(crate) async fn list(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Ok(scoped());
    }
    let caller = session.person_id.ok_or(ApiError::NotFound)?;
    let owner = query.person.unwrap_or(caller);
    if owner != caller
        && !keys::may_revoke(&state.surreal, session.role, Some(caller), owner)
            .await
            .unwrap_or(false)
    {
        return Err(ApiError::NotFound);
    }
    let Ok(pg) = PostgresDb::from_env().await else {
        return Ok(unavailable());
    };
    match keys::list_for_person(&pg, owner).await {
        Ok(listed) => Ok(Json(serde_json::json!({ "keys": listed })).into_response()),
        Err(error) => Ok(refused(&error)),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddKey {
    /// One OpenSSH public-key line.
    public_key: String,
    /// Optional label; defaults to the key's comment.
    #[serde(default)]
    label: Option<String>,
}

/// Register a key for the caller, then reconcile their Forgejo account.
async fn add_for_session(
    surreal: &store::surreal::SurrealDb,
    session: &SessionData,
    public_key: &str,
    label: Option<&str>,
) -> Result<(SshPublicKey, serde_json::Value), Response> {
    let person = session
        .person_id
        .ok_or_else(|| json_error(StatusCode::FORBIDDEN, "no_person", "no linked Person"))?;
    // Parse first, so a malformed or private key is refused before any
    // store is touched.
    keys::parse(public_key).map_err(|error| refused(&error))?;
    keys::require_firm_member(surreal, person)
        .await
        .map_err(|error| refused(&error))?;
    let pg = PostgresDb::from_env().await.map_err(|_| unavailable())?;
    let key = keys::add(&pg, person, public_key, label)
        .await
        .map_err(|error| refused(&error))?;
    let outcome = sync(surreal, &pg, person).await;
    Ok((key, outcome))
}

/// `POST /app/api/ssh-keys` — register one of the caller's own keys.
pub(crate) async fn add(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Json(input): Json<AddKey>,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Ok(scoped());
    }
    match add_for_session(
        &state.surreal,
        &session,
        &input.public_key,
        input.label.as_deref(),
    )
    .await
    {
        Ok((key, outcome)) => Ok((
            StatusCode::CREATED,
            Json(serde_json::json!({ "key": key, "sync": outcome })),
        )
            .into_response()),
        Err(response) => Ok(response),
    }
}

/// Revoke a key the caller owns or administers, then reconcile its owner's
/// Forgejo account. A key outside the caller's reach is a 404.
async fn revoke_for_session(
    surreal: &store::surreal::SurrealDb,
    session: &SessionData,
    id: Uuid,
) -> Result<(SshPublicKey, serde_json::Value), Response> {
    let not_found = || refused(&SshKeyError::NotFound);
    let pg = PostgresDb::from_env().await.map_err(|_| unavailable())?;
    let key = keys::find(&pg, id)
        .await
        .map_err(|error| refused(&error))?
        .ok_or_else(not_found)?;
    let allowed = keys::may_revoke(surreal, session.role, session.person_id, key.person_id)
        .await
        .unwrap_or(false);
    if !allowed {
        return Err(not_found());
    }
    let revoked = keys::revoke(&pg, id, session.person_id)
        .await
        .map_err(|error| refused(&error))?;
    let outcome = sync(surreal, &pg, revoked.person_id).await;
    Ok((revoked, outcome))
}

/// `DELETE /app/api/ssh-keys/{id}` — revoke a key.
pub(crate) async fn revoke(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Ok(scoped());
    }
    match revoke_for_session(&state.surreal, &session, id).await {
        Ok((key, outcome)) => {
            Ok(Json(serde_json::json!({ "key": key, "sync": outcome })).into_response())
        }
        Err(response) => Ok(response),
    }
}

/// `POST /app/api/ssh-keys/sync` — reconcile the caller's own Forgejo
/// account now, for example after their first Forgejo sign-in.
pub(crate) async fn sync_own(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
) -> Result<Response, ApiError> {
    if session.scope.is_some() {
        return Ok(scoped());
    }
    let person = session.person_id.ok_or(ApiError::NotFound)?;
    let Ok(pg) = PostgresDb::from_env().await else {
        return Ok(unavailable());
    };
    let outcome = sync(&state.surreal, &pg, person).await;
    Ok(Json(serde_json::json!({ "sync": outcome })).into_response())
}

// ---- The profile page's native forms ----

/// Back to the profile page with a fixed flash code (never request text).
fn back_to_profile(flash: &str, code: &str) -> Response {
    Redirect::to(&format!(
        "{}?{flash}={code}#profile-ssh-keys",
        webapp::profile::PROFILE_PATH,
    ))
    .into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddKeyForm {
    #[serde(default)]
    public_key: String,
    #[serde(default)]
    label: String,
}

/// `POST /app/profile/ssh-keys` — the profile page's add form.
async fn profile_add(
    State(state): State<AdminState>,
    session: Option<Extension<SessionData>>,
    Form(input): Form<AddKeyForm>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let label = Some(input.label.as_str()).filter(|label| !label.trim().is_empty());
    match add_for_session(&state.surreal, &session, &input.public_key, label).await {
        Ok(_) => back_to_profile("ssh_key", "added"),
        Err(response) => back_to_profile("ssh_key_error", flash_code(response.status())),
    }
}

/// `POST /app/profile/ssh-keys/{id}/delete` — the profile page's remove
/// button for one of the caller's own keys.
async fn profile_remove(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    session: Option<Extension<SessionData>>,
) -> Response {
    let Some(Extension(session)) = session else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match revoke_for_session(&state.surreal, &session, id).await {
        Ok(_) => back_to_profile("ssh_key", "removed"),
        Err(response) => back_to_profile("ssh_key_error", flash_code(response.status())),
    }
}

/// A short, stable flash code for the profile page; the page maps it to a
/// sentence. The status is enough: the API carries the detail.
fn flash_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNPROCESSABLE_ENTITY => "invalid",
        StatusCode::CONFLICT => "duplicate",
        StatusCode::FORBIDDEN => "not_member",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        _ => "failed",
    }
}

/// The profile page's native-form routes, merged into `admin::routes`.
/// `/app/profile/**` is admitted for every authenticated tier by the route
/// policy; these handlers decide the rest.
pub fn routes() -> Router<AdminState> {
    Router::new()
        .route(PROFILE_SSH_KEYS_PATH, post(profile_add))
        .route("/app/profile/ssh-keys/{id}/delete", post(profile_remove))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_to_stable_wire_codes() {
        assert_eq!(
            refusal(&SshKeyError::PrivateKey),
            (StatusCode::UNPROCESSABLE_ENTITY, "private_key")
        );
        assert_eq!(
            refusal(&SshKeyError::Duplicate),
            (StatusCode::CONFLICT, "duplicate_key")
        );
        assert_eq!(
            refusal(&SshKeyError::NotFirmMember),
            (StatusCode::FORBIDDEN, "not_firm_member")
        );
        assert_eq!(
            refusal(&SshKeyError::NotFound),
            (StatusCode::NOT_FOUND, "not_found")
        );
    }

    #[test]
    fn profile_flash_codes_name_the_refusal_class() {
        assert_eq!(flash_code(StatusCode::UNPROCESSABLE_ENTITY), "invalid");
        assert_eq!(flash_code(StatusCode::CONFLICT), "duplicate");
        assert_eq!(flash_code(StatusCode::FORBIDDEN), "not_member");
        assert_eq!(flash_code(StatusCode::INTERNAL_SERVER_ERROR), "failed");
    }
}
