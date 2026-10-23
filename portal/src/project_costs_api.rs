//! Receipt-backed Project cost reads and commands.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{persons::Role, project_costs as costs};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, AuthedSession, LawyerSession};

fn error(
    error: &costs::ProjectCostError,
    operation: &'static str,
    project_id: Uuid,
    cost_id: Option<Uuid>,
) -> Response {
    let status = match error {
        costs::ProjectCostError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        costs::ProjectCostError::Conflict | costs::ProjectCostError::ReceiptTaken => {
            StatusCode::CONFLICT
        }
        costs::ProjectCostError::NotFound => StatusCode::NOT_FOUND,
        costs::ProjectCostError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "project_cost_operation_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            cost_id = ?cost_id
        );
    }
    (
        status,
        Json(serde_json::json!({"error":"project_cost_command_refused"})),
    )
        .into_response()
}

async fn ensure_project_access(
    state: &ApiState,
    person: Option<Uuid>,
    role: Role,
    project_id: Uuid,
    lawyer_only: bool,
) -> Result<(), ApiError> {
    let Some(person) = person else {
        return Err(ApiError::NotFound);
    };
    let visible = if lawyer_only {
        store::access::can_see_project_as_lawyer(&state.surreal, Some(person), role, project_id)
            .await
            .map_err(ApiError::Db)?
    } else {
        store::access::can_see_project(&state.surreal, Some(person), role, project_id)
            .await
            .map_err(ApiError::Db)?
    };
    if visible {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

async fn postgres(
    operation: &'static str,
    project_id: Uuid,
    cost_id: Option<Uuid>,
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "project_cost_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id,
            cost_id = ?cost_id
        );
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"postgres_cost_store_unavailable"})),
        )
            .into_response()
    })
}

/// `GET /app/api/projects/{id}/costs`; clients see only costs whose receipt remains client-visible.
pub(crate) async fn list(
    State(state): State<ApiState>,
    AuthedSession(session): AuthedSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    ensure_project_access(&state, session.person_id, session.role, project_id, false).await?;
    let pg = match postgres("list", project_id, None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let rows = if session.role == Role::Client {
        costs::list_client_visible(&state.surreal, &pg, project_id).await
    } else {
        costs::list_for_project(&pg, project_id).await
    };
    match rows {
        Ok(rows) => {
            Ok(Json(serde_json::json!({"costs":rows,"totals":totals(&rows)})).into_response())
        }
        Err(e) => Ok(error(&e, "list", project_id, None)),
    }
}

fn totals(rows: &[costs::ProjectCost]) -> Vec<serde_json::Value> {
    let mut grouped = std::collections::BTreeMap::<(String, String), i64>::new();
    for row in rows {
        let total = grouped
            .entry((row.currency.clone(), row.amount_status.clone()))
            .or_default();
        *total = total.saturating_add(row.amount_cents);
    }
    grouped.into_iter().map(|((currency, amount_status), amount_cents)| serde_json::json!({"currency":currency,"amount_status":amount_status,"amount_cents":amount_cents})).collect()
}

/// `GET /app/api/projects/{id}/costs/{cost_id}/events` — lawyer-only audit history.
pub(crate) async fn events(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, cost_id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let pg = match postgres("events", project_id, Some(cost_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let cost = match costs::find_by_id(&pg, cost_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "events", project_id, Some(cost_id))),
    };
    if cost.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    ensure_project_access(&state, session.person_id, session.role, project_id, true).await?;
    Ok(match costs::list_events(&pg, cost_id).await {
        Ok(events) => Json(serde_json::json!({"events":events})).into_response(),
        Err(e) => error(&e, "events", project_id, Some(cost_id)),
    })
}

/// `POST /app/api/projects/{id}/costs`.
pub(crate) async fn create(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<costs::NewProjectCost>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_project_access(&state, session.person_id, session.role, project_id, true).await?;
    let pg = match postgres("create", project_id, None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match costs::create(&state.surreal, &pg, project_id, actor_id, &input).await {
        Ok(row) => Ok((StatusCode::CREATED, Json(row)).into_response()),
        Err(e) => Ok(error(&e, "create", project_id, None)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfirmInput {
    amount_cents: i64,
}
pub(crate) async fn confirm(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, cost_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ConfirmInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let pg = match postgres("confirm", project_id, Some(cost_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let before = match costs::find_by_id(&pg, cost_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "confirm", project_id, Some(cost_id))),
    };
    if before.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    ensure_project_access(&state, session.person_id, session.role, project_id, true).await?;
    Ok(
        match costs::confirm_amount(&pg, cost_id, actor_id, input.amount_cents).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => error(&e, "confirm", project_id, Some(cost_id)),
        },
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvoiceInput {
    invoice_id: String,
}
pub(crate) async fn link_invoice(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, cost_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<InvoiceInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let pg = match postgres("link_invoice", project_id, Some(cost_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let before = match costs::find_by_id(&pg, cost_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "link_invoice", project_id, Some(cost_id))),
    };
    if before.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    ensure_project_access(&state, session.person_id, session.role, project_id, true).await?;
    Ok(
        match costs::link_invoice(&state.surreal, &pg, cost_id, actor_id, &input.invoice_id).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => error(&e, "link_invoice", project_id, Some(cost_id)),
        },
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveInput {
    reason: String,
}
pub(crate) async fn resolve(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, cost_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ResolveInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let pg = match postgres("resolve", project_id, Some(cost_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let before = match costs::find_by_id(&pg, cost_id).await {
        Ok(row) => row,
        Err(e) => return Ok(error(&e, "resolve", project_id, Some(cost_id))),
    };
    if before.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    ensure_project_access(&state, session.person_id, session.role, project_id, true).await?;
    Ok(
        match costs::resolve(&pg, cost_id, actor_id, &input.reason).await {
            Ok(row) => Json(row).into_response(),
            Err(e) => error(&e, "resolve", project_id, Some(cost_id)),
        },
    )
}
