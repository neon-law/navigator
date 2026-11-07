//! Firm capability-gated assumption commands. The catalog of keys is public;
//! the values are Firm data in PostgreSQL and never leave it.
use super::{ApiError, ApiState, LawyerSession};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::firm_assumptions::{self, FirmAssumptionError, NewFirmAssumption};
use uuid::Uuid;

async fn authorize(
    state: &ApiState,
    session: &crate::SessionData,
    firm: Uuid,
) -> Result<(), ApiError> {
    let allowed = store::firm_capability::FirmCapability::ManageFirmAssumptions
        .resolve(&state.surreal, session.role, session.person_id, firm)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    if !allowed.is_allowed() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// The configured assumption store. Assumptions are PostgreSQL-native; an
/// unreachable or unconfigured PostgreSQL fails the request rather than
/// answering from anywhere else.
async fn assumption_store() -> Result<store::postgres::PostgresDb, ApiError> {
    store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ApiError::Db("PostgreSQL assumptions unavailable".into()))
}

fn assumption_error(error: &FirmAssumptionError) -> Response {
    let status = match error {
        FirmAssumptionError::Invalid(_)
        | FirmAssumptionError::UnknownKey(_)
        | FirmAssumptionError::TypeMismatch => StatusCode::UNPROCESSABLE_ENTITY,
        FirmAssumptionError::Catalog => StatusCode::INTERNAL_SERVER_ERROR,
        FirmAssumptionError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(serde_json::json!({"error": "assumption_refused"}))).into_response()
}

/// The Firm's current assumptions: one row per key, the newest version of
/// each, ordered by key. Another Firm's rows never appear.
pub(super) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let pg = assumption_store().await?;
    match firm_assumptions::list(&pg, firm).await {
        Ok(assumptions) => Ok(Json(assumptions).into_response()),
        Err(e) => Ok(assumption_error(&e)),
    }
}

/// The newest version of one key, or 404 when the Firm has never set it —
/// the blank a fresh deployment renders.
pub(super) async fn current(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((firm, key)): Path<(Uuid, String)>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let pg = assumption_store().await?;
    match firm_assumptions::current(&pg, firm, &key).await {
        Ok(Some(assumption)) => Ok(Json(assumption).into_response()),
        Ok(None) => Err(ApiError::NotFound),
        Err(e) => Ok(assumption_error(&e)),
    }
}

/// Every version of one key, newest first — the append-only history.
pub(super) async fn history(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((firm, key)): Path<(Uuid, String)>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let pg = assumption_store().await?;
    match firm_assumptions::history(&pg, firm, &key).await {
        Ok(assumptions) => Ok(Json(assumptions).into_response()),
        Err(e) => Ok(assumption_error(&e)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetAssumptionBody {
    value: serde_json::Value,
    reason: Option<String>,
}

/// Records a new version of one key; the acting person is the session's.
pub(super) async fn set(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((firm, key)): Path<(Uuid, String)>,
    Json(body): Json<SetAssumptionBody>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let pg = assumption_store().await?;
    let input = NewFirmAssumption {
        key,
        value: body.value,
        reason: body.reason,
    };
    match firm_assumptions::set(&pg, firm, session.person_id.ok_or(ApiError::Forbidden)?, &input)
        .await
    {
        Ok(assumption) => Ok(Json(assumption).into_response()),
        Err(e) => Ok(assumption_error(&e)),
    }
}

/// The public catalog of known keys with their types and null defaults —
/// no Firm values, so this reads the embedded catalog, not the database.
pub(super) async fn catalog() -> Response {
    let keys: Vec<serde_json::Value> = firm_assumptions::catalog()
        .iter()
        .map(|(key, spec)| {
            serde_json::json!({
                "key": key,
                "type": spec.kind.as_str(),
                "default": spec.default,
                "description": spec.description,
            })
        })
        .collect();
    Json(keys).into_response()
}
