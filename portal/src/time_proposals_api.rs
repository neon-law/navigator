//! `navigator project time`: record, review, and backfill time from the CLI
//! (LAW-583, LAW-584). Time Entry Proposals wait for a lawyer's approval,
//! which prices them into Time Entries; staged Jujutsu time is reviewed
//! through the same doors.
//!
//! Admission is lawyer tier plus an embedded-Rego `route_allow` rule; the
//! matter question is asked once through [`store::access::matter_viewer`], so
//! a caller without firm-side participation gets the non-disclosing 404.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use serde::Deserialize;
use store::time_proposals::{
    self, Approval, NewProposal, ProposalEdit, ProposalError, StatusFilter,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::session::SessionData;

fn refusal(error: &ProposalError, operation: &'static str, project: Uuid) -> Response {
    use store::time_entries::TimeEntryError;
    let status = match error {
        ProposalError::Invalid(_)
        | ProposalError::NotParticipant
        | ProposalError::Time(TimeEntryError::Invalid(_) | TimeEntryError::NotParticipant) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ProposalError::NotFound | ProposalError::Time(TimeEntryError::NotFound) => {
            StatusCode::NOT_FOUND
        }
        ProposalError::Decided(_)
        | ProposalError::RequestReused
        | ProposalError::Time(TimeEntryError::Conflict) => StatusCode::CONFLICT,
        ProposalError::Time(TimeEntryError::Postgres(_)) => StatusCode::SERVICE_UNAVAILABLE,
        ProposalError::Time(TimeEntryError::Database(_)) | ProposalError::Database(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let message = if status.is_server_error() {
        tracing::warn!(
            event = "time_proposal_operation_failed",
            operation,
            project_id = %project,
            store_error = %error
        );
        "time storage failed".to_owned()
    } else {
        error.to_string()
    };
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

/// The matter's firm-side lawyer, never an impersonated view for a write.
async fn authorize(
    state: &ApiState,
    session: &SessionData,
    project: Uuid,
    write: bool,
) -> Result<Uuid, ApiError> {
    if write && session.viewing_as_dri.is_some() {
        return Err(ApiError::Forbidden);
    }
    let actor = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer =
        store::access::matter_viewer(&state.application_store, Some(actor), session.role, project)
            .await
            .map_err(ApiError::Db)?;
    match viewer {
        Some(viewer) if viewer.is_firm_side() => Ok(actor),
        _ => Err(ApiError::NotFound),
    }
}

async fn store(
    operation: &'static str,
    project: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    crate::time_tracking_api::postgres(operation, Some(project)).await
}

fn reply<T: serde::Serialize>(
    result: Result<T, ProposalError>,
    created: bool,
    operation: &'static str,
    project: Uuid,
) -> Response {
    match result {
        Ok(value) if created => (StatusCode::CREATED, Json(value)).into_response(),
        Ok(value) => Json(value).into_response(),
        Err(error) => refusal(&error, operation, project),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    status: Option<StatusFilter>,
}

/// `GET .../time-entry-proposals` — pending proposals and staged Jujutsu
/// time, discarded proposals, and approved Time Entries, oldest work first.
pub(crate) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project): Path<Uuid>,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, project, false).await?;
    let pg = match store("list_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    Ok(reply(
        time_proposals::list(&pg, project, query.status).await,
        false,
        "list_time",
        project,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AddInput {
    request_id: Uuid,
    performed_on: NaiveDate,
    minutes: i32,
    narrative: String,
    /// A Person UUID or email. Defaults to the caller.
    #[serde(default)]
    timekeeper: Option<String>,
}

async fn timekeeper(
    state: &ApiState,
    actor: Uuid,
    named: Option<&str>,
) -> Result<Result<Uuid, Response>, ApiError> {
    let Some(named) = named.map(str::trim).filter(|named| !named.is_empty()) else {
        return Ok(Ok(actor));
    };
    if let Ok(id) = Uuid::parse_str(named) {
        return Ok(Ok(id));
    }
    let person = store::persons::find_by_email_ci(&state.application_store, named)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?;
    Ok(person.map(|person| person.id).ok_or_else(|| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "no Person has that timekeeper email"})),
        )
            .into_response()
    }))
}

/// `POST .../time-entry-proposals` — record time for review, past dates
/// allowed. A replayed `request_id` returns the first row.
pub(crate) async fn add(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project): Path<Uuid>,
    Json(input): Json<AddInput>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &session, project, true).await?;
    let timekeeper_id = match timekeeper(&state, actor, input.timekeeper.as_deref()).await? {
        Ok(id) => id,
        Err(response) => return Ok(response),
    };
    let pg = match store("add_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let proposal = NewProposal {
        request_id: input.request_id,
        timekeeper_id,
        performed_on: input.performed_on,
        minutes: input.minutes,
        narrative: input.narrative,
    };
    Ok(reply(
        time_proposals::add(&pg, project, actor, &proposal).await,
        true,
        "add_time",
        project,
    ))
}

/// `PATCH .../time-entry-proposals/{proposal_id}` — change a pending
/// proposal's date, minutes, or narrative.
pub(crate) async fn edit(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(change): Json<ProposalEdit>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, project, true).await?;
    let pg = match store("edit_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    Ok(reply(
        time_proposals::edit(&pg, project, id, &change).await,
        false,
        "edit_time",
        project,
    ))
}

/// `POST .../time-entry-proposals/{proposal_id}/approve` — price and record
/// the reviewed time as a Time Entry.
pub(crate) async fn approve(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(approval): Json<Approval>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &session, project, true).await?;
    let pg = match store("approve_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    Ok(reply(
        time_proposals::approve(&pg, project, actor, id, &approval).await,
        false,
        "approve_time",
        project,
    ))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscardInput {
    #[serde(default)]
    reason: Option<String>,
}

/// `POST .../time-entry-proposals/{proposal_id}/discard` — discard a pending
/// proposal or deny staged Jujutsu time.
pub(crate) async fn discard(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<DiscardInput>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &session, project, true).await?;
    let pg = match store("discard_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    Ok(reply(
        time_proposals::discard(&pg, project, actor, id, input.reason.as_deref()).await,
        false,
        "discard_time",
        project,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackfillInput {
    #[serde(default)]
    since: Option<NaiveDate>,
    #[serde(default)]
    dry_run: bool,
}

/// `POST .../time-entry-proposals/backfill` — propose Time Entries from the
/// Project's dated record. Never approves.
pub(crate) async fn backfill(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project): Path<Uuid>,
    Json(input): Json<BackfillInput>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &session, project, !input.dry_run).await?;
    let pg = match store("backfill_time", project).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    Ok(reply(
        time_proposals::backfill(&pg, project, actor, input.since, input.dry_run).await,
        false,
        "backfill_time",
        project,
    ))
}
