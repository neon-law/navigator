//! Attorney time tracking over the PostgreSQL store: the Billing tab's time
//! doors — Time Entries (manual logging, invoice marking, write-offs), the
//! durable Timer (start, stop, discard), and the fee structures that price
//! new work (hourly rates with matter overrides, matter flat fees).
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule), exactly as [`crate::project_tasks_api`];
//! the matter-scope question is asked once through
//! [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row — a client, a clerk, an unassigned lawyer, Owner and
//! Admin included — gets the non-disclosing 404, never a refusal. The rate
//! doors are firm-tier: every lawyer may read and record the firm's rates.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDate;
use serde::Deserialize;
use store::{attorney_rates, postgres_mirror, time_entries, timers};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

/// The wire shape for a Time Entry: flat lowercase words for `source` and
/// `billing_status` (the store's `as_str`, the same convention as the
/// project-costs API), with the status's evidence column alongside — never
/// the store's internally-tagged serde enums.
fn entry_view(row: &time_entries::TimeEntry) -> serde_json::Value {
    let mut view = serde_json::json!({
        "id": row.id,
        "project_id": row.project_id,
        "attorney_id": row.attorney_id,
        "performed_on": row.performed_on,
        "minutes": row.minutes,
        "description": row.description,
        "billable": row.billable,
        "rate_cents_per_hour": row.rate_cents_per_hour,
        "source": row.source.as_str(),
        "billing_status": row.billing_status.as_str(),
        "inserted_at": row.inserted_at,
        "updated_at": row.updated_at,
    });
    match &row.billing_status {
        time_entries::BillingStatus::Invoiced { xero_invoice_id } => {
            view["xero_invoice_id"] = serde_json::json!(xero_invoice_id);
        }
        time_entries::BillingStatus::WrittenOff { reason } => {
            view["written_off_reason"] = serde_json::json!(reason);
        }
        // Unbilled: nothing extra to show. Reserved: drawn into a pending
        // billing batch — nothing extra in the entry view until it settles.
        time_entries::BillingStatus::Unbilled | time_entries::BillingStatus::Reserved => {}
    }
    view
}

fn refused(status: StatusCode) -> Response {
    (
        status,
        Json(serde_json::json!({"error": "time_tracking_command_refused"})),
    )
        .into_response()
}

fn entry_error(
    error: &time_entries::TimeEntryError,
    operation: &'static str,
    project_id: Uuid,
    entry_id: Option<Uuid>,
) -> Response {
    let status = match error {
        time_entries::TimeEntryError::Invalid(_) | time_entries::TimeEntryError::NotParticipant => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        time_entries::TimeEntryError::Conflict => StatusCode::CONFLICT,
        time_entries::TimeEntryError::NotFound => StatusCode::NOT_FOUND,
        time_entries::TimeEntryError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        time_entries::TimeEntryError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "time_entry_operation_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            entry_id = ?entry_id,
            store_error = %error
        );
    }
    refused(status)
}

fn timer_error(error: &timers::TimerError, operation: &'static str, project_id: Uuid) -> Response {
    let status = match error {
        timers::TimerError::Invalid(_) | timers::TimerError::NotParticipant => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        timers::TimerError::Conflict => StatusCode::CONFLICT,
        // Another attorney's Timer does not exist for the caller: the same
        // non-disclosing posture the matter surfaces hold.
        timers::TimerError::NotOwner | timers::TimerError::NotFound => StatusCode::NOT_FOUND,
        timers::TimerError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        // A fault in the store itself, or in the entry the capture composed:
        // a server error, and the Timer stays put.
        timers::TimerError::Entry(_) | timers::TimerError::Database(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "timer_operation_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %error
        );
    }
    refused(status)
}

fn fee_error(error: &attorney_rates::FeeStructureError) -> Response {
    let status = match error {
        attorney_rates::FeeStructureError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        attorney_rates::FeeStructureError::Postgres(_) => StatusCode::SERVICE_UNAVAILABLE,
        attorney_rates::FeeStructureError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "fee_structure_operation_failed",
            outcome = "store_error",
            store_error = %error
        );
    }
    refused(status)
}

/// The matter surface's own question, asked once: `store::access` requires the
/// participation row of every tier (ENG-81), and time tracking is a firm-side
/// surface, so only the Lawyer and LawyerDri viewers pass. Everyone else —
/// including a client-side participant and an unassigned Owner — collapses to
/// the non-disclosing 404.
async fn ensure_firm_side(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: store::persons::Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let viewer = store::access::matter_viewer(&state.surreal, person_id, role, project_id)
        .await
        .map_err(ApiError::Db)?;
    match viewer {
        Some(viewer) if viewer.is_firm_side() => Ok(()),
        _ => Err(ApiError::NotFound),
    }
}

async fn postgres(
    operation: &'static str,
    project_id: Option<Uuid>,
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "time_tracking_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = ?project_id
        );
        refused(StatusCode::SERVICE_UNAVAILABLE)
    })
}

/// Open the time-tracking tables with their dependency rows mirrored: the
/// entry, timer, and flat-fee commands all check the parent Project row and
/// the attorney's participation in PostgreSQL, so a Project or participant
/// added before the mirror was enabled would otherwise be wrongly refused
/// there.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    let pg = postgres(operation, Some(project_id)).await?;
    let mirrored = async {
        postgres_mirror::mirror(
            &state.surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await?;
        let participations =
            store::projects::participations_for_project(&state.surreal, project_id)
                .await
                .map_err(|_| postgres_mirror::MirrorError::Source)?;
        for role in participations {
            postgres_mirror::mirror(
                &state.surreal,
                &pg,
                &postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await?;
        }
        Ok::<_, postgres_mirror::MirrorError>(())
    }
    .await;
    if let Err(store_error) = mirrored {
        tracing::warn!(
            event = "time_tracking_mirror_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %store_error
        );
        return Err(refused(StatusCode::SERVICE_UNAVAILABLE));
    }
    Ok(pg)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewEntryInput {
    /// A participant on the same Project; refused otherwise.
    attorney_id: Uuid,
    performed_on: NaiveDate,
    /// Whole minutes of work, 1..=1440.
    minutes: i32,
    description: String,
    billable: bool,
}

/// The rate snapshot for a new entry: the attorney's fee structure resolved
/// for `performed_on` when the work is billable — a matter override over the
/// firm default, and no rate at all is a refusal, never a silent $0 — and
/// zero for non-billable work, which never carries a price.
async fn snapshot_rate(
    pg: &store::postgres::PostgresDb,
    attorney_id: Uuid,
    project_id: Uuid,
    performed_on: NaiveDate,
    billable: bool,
) -> Result<i32, Response> {
    if !billable {
        return Ok(0);
    }
    match attorney_rates::resolve_hourly_rate_cents(pg, attorney_id, project_id, performed_on).await
    {
        Ok(Some(rate_cents_per_hour)) => Ok(rate_cents_per_hour),
        Ok(None) => Err(refused(StatusCode::UNPROCESSABLE_ENTITY)),
        Err(e) => Err(fee_error(&e)),
    }
}

/// `GET /app/api/projects/{id}/time-entries` — the matter's time entries,
/// newest work first, each carrying its billing status and the rate
/// snapshotted at creation.
pub(crate) async fn list_entries(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("list_entries", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match time_entries::list_for_project(&pg, project_id).await {
        Ok(rows) => Ok(Json(serde_json::json!({
            "time_entries": rows.iter().map(entry_view).collect::<Vec<_>>()
        }))
        .into_response()),
        Err(e) => Ok(entry_error(&e, "list_entries", project_id, None)),
    }
}

/// `POST /app/api/projects/{id}/time-entries` — log time by hand. The rate is
/// never taken from the wire: it is resolved from the attorney's fee
/// structure for the performed date and frozen onto the row.
pub(crate) async fn create_entry(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<NewEntryInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "create_entry", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let rate_cents_per_hour = match snapshot_rate(
        &pg,
        input.attorney_id,
        project_id,
        input.performed_on,
        input.billable,
    )
    .await
    {
        Ok(rate) => rate,
        Err(response) => return Ok(response),
    };
    let entry = time_entries::NewTimeEntry {
        attorney_id: input.attorney_id,
        performed_on: input.performed_on,
        minutes: input.minutes,
        description: input.description,
        billable: input.billable,
        rate_cents_per_hour,
        source: time_entries::TimeEntrySource::Manual,
    };
    match time_entries::create(&pg, project_id, actor_id, &entry).await {
        Ok(row) => Ok((StatusCode::CREATED, Json(entry_view(&row))).into_response()),
        Err(e) => Ok(entry_error(&e, "create_entry", project_id, None)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InvoiceInput {
    xero_invoice_id: String,
}

/// `POST /app/api/projects/{id}/time-entries/{entry_id}/invoice` — mark one
/// unbilled entry `invoiced` under the Xero invoice that carries it. Replaying
/// the same call is a no-op; the same entry under a different invoice id is a
/// conflict (409).
pub(crate) async fn mark_invoiced(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, entry_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<InvoiceInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("mark_invoiced", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let entry = match time_entries::find_by_id(&pg, entry_id).await {
        Ok(row) => row,
        Err(e) => return Ok(entry_error(&e, "mark_invoiced", project_id, Some(entry_id))),
    };
    if entry.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    match time_entries::mark_invoiced(&pg, &[entry_id], actor_id, &input.xero_invoice_id, false)
        .await
    {
        Ok(mut rows) => Ok(Json(entry_view(&rows.remove(0))).into_response()),
        Err(e) => Ok(entry_error(&e, "mark_invoiced", project_id, Some(entry_id))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteOffInput {
    reason: String,
}

/// `POST /app/api/projects/{id}/time-entries/{entry_id}/write-off` — resolve
/// an unbilled entry as a deliberate write-off, retaining the reason. The row
/// is retained; a resolved entry never reopens.
pub(crate) async fn write_off(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project_id, entry_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<WriteOffInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("write_off", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let entry = match time_entries::find_by_id(&pg, entry_id).await {
        Ok(row) => row,
        Err(e) => return Ok(entry_error(&e, "write_off", project_id, Some(entry_id))),
    };
    if entry.project_id != project_id {
        return Err(ApiError::NotFound);
    }
    match time_entries::write_off(&pg, entry_id, actor_id, &input.reason).await {
        Ok(row) => Ok(Json(entry_view(&row)).into_response()),
        Err(e) => Ok(entry_error(&e, "write_off", project_id, Some(entry_id))),
    }
}

/// The calling attorney's open Timer when it runs on this matter. A Timer
/// running on another matter is not named here — the caller stops or discards
/// it from its own matter, and this door discloses nothing about it.
fn timer_on_matter(running: Option<timers::Timer>, project_id: Uuid) -> Option<timers::Timer> {
    running.filter(|timer| timer.project_id == project_id)
}

/// `GET /app/api/projects/{id}/timer` — the calling attorney's open Timer on
/// this matter, if any: the recovery read behind the matter header on every
/// page. The elapsed time is computed from `started_at`, so a closed tab
/// loses nothing.
pub(crate) async fn running_timer(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("running_timer", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match timers::running_for_person(&pg, actor_id).await {
        Ok(running) => {
            let timer = timer_on_matter(running, project_id);
            Ok(Json(serde_json::json!({ "timer": timer })).into_response())
        }
        Err(e) => Ok(timer_error(&e, "running_timer", project_id)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartTimerInput {
    note: Option<String>,
}

/// `POST /app/api/projects/{id}/timer` — start the calling attorney's single
/// firm-wide Timer on this matter. A second open Timer, on any matter, is a
/// conflict (409); the durable row exists the moment this returns.
pub(crate) async fn start_timer(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<StartTimerInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "start_timer", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match timers::start_timer(&pg, actor_id, project_id, input.note).await {
        Ok(timer) => Ok((StatusCode::CREATED, Json(timer)).into_response()),
        Err(e) => Ok(timer_error(&e, "start_timer", project_id)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StopTimerInput {
    description: String,
    billable: bool,
}

/// `POST /app/api/projects/{id}/timer/stop` — stop the calling attorney's
/// open Timer on this matter and capture its elapsed minutes as a Time Entry
/// in one transaction. The rate is resolved from the attorney's fee structure
/// for the date the Timer started and frozen onto the entry; minutes are the
/// store's own computation and never the wire's.
pub(crate) async fn stop_timer(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<StopTimerInput>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "stop_timer", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let running = match timers::running_for_person(&pg, actor_id).await {
        Ok(running) => running,
        Err(e) => return Ok(timer_error(&e, "stop_timer", project_id)),
    };
    let Some(timer) = timer_on_matter(running, project_id) else {
        return Ok(refused(StatusCode::NOT_FOUND));
    };
    let rate_cents_per_hour = match snapshot_rate(
        &pg,
        actor_id,
        project_id,
        timer.started_at.date_naive(),
        input.billable,
    )
    .await
    {
        Ok(rate) => rate,
        Err(response) => return Ok(response),
    };
    let entry = time_entries::NewTimeEntry {
        attorney_id: actor_id,
        // stop_timer stamps these from the Timer itself: the elapsed minutes,
        // the Timer source, and the date the Timer started.
        performed_on: timer.started_at.date_naive(),
        minutes: 0,
        description: input.description,
        billable: input.billable,
        rate_cents_per_hour,
        source: time_entries::TimeEntrySource::Timer,
    };
    match timers::stop_timer(&pg, timer.id, actor_id, entry).await {
        Ok(captured) => Ok(Json(entry_view(&captured)).into_response()),
        Err(e) => Ok(timer_error(&e, "stop_timer", project_id)),
    }
}

/// `POST /app/api/projects/{id}/timer/discard` — discard the calling
/// attorney's open Timer on this matter without recording anything.
/// Deliberate, owner-only, and transactional.
pub(crate) async fn discard_timer(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("discard_timer", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let running = match timers::running_for_person(&pg, actor_id).await {
        Ok(running) => running,
        Err(e) => return Ok(timer_error(&e, "discard_timer", project_id)),
    };
    let Some(timer) = timer_on_matter(running, project_id) else {
        return Ok(refused(StatusCode::NOT_FOUND));
    };
    match timers::discard_timer(&pg, timer.id, actor_id).await {
        Ok(()) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(e) => Ok(timer_error(&e, "discard_timer", project_id)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RatesQuery {
    person_id: Uuid,
}

/// `GET /app/api/attorney-rates?person_id={id}` — the firm's rate history for
/// one attorney: firm defaults and matter overrides together, newest
/// effective date first.
pub(crate) async fn list_rates(
    State(_state): State<ApiState>,
    LawyerSession(_session): LawyerSession,
    Query(query): Query<RatesQuery>,
) -> Result<Response, ApiError> {
    let pg = match postgres("list_rates", None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match attorney_rates::rates_for_person(&pg, query.person_id).await {
        Ok(rows) => Ok(Json(serde_json::json!({"rates": rows})).into_response()),
        Err(e) => Ok(fee_error(&e)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewRateInput {
    person_id: Uuid,
    /// `None` records the firm default; a matter id records the override.
    project_id: Option<Uuid>,
    hourly_rate_cents: i32,
    effective_on: NaiveDate,
}

/// `POST /app/api/attorney-rates` — record an attorney's hourly rate, either
/// the firm default (`project_id: null`) or a matter override. Rows are
/// effective-dated and never edited: a row for the same scope and effective
/// date replaces the prior one, any other date appends to the history.
pub(crate) async fn create_rate(
    State(state): State<ApiState>,
    LawyerSession(_session): LawyerSession,
    Json(input): Json<NewRateInput>,
) -> Result<Response, ApiError> {
    let pg = match postgres("create_rate", None).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    // The rate rows carry foreign keys into the mirrored identity and
    // project tables; mirror what the row names first, and refuse an
    // unknown attorney or matter with the non-disclosing 404 rather than a
    // constraint fault.
    let person_exists = store::persons::find_by_id(&state.surreal, input.person_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .is_some();
    if !person_exists {
        return Err(ApiError::NotFound);
    }
    if let Some(project_id) = input.project_id {
        let project_exists = store::projects::find_by_id(&state.surreal, project_id)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?
            .is_some();
        if !project_exists {
            return Err(ApiError::NotFound);
        }
        if let Err(store_error) = postgres_mirror::mirror(
            &state.surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await
        {
            tracing::warn!(
                event = "fee_structure_mirror_failed",
                operation = "create_rate",
                outcome = "store_error",
                store_error = %store_error
            );
            return Ok(refused(StatusCode::SERVICE_UNAVAILABLE));
        }
    }
    if let Err(store_error) = postgres_mirror::mirror(
        &state.surreal,
        &pg,
        &postgres_mirror::MirrorKey::Person(input.person_id),
    )
    .await
    {
        tracing::warn!(
            event = "fee_structure_mirror_failed",
            operation = "create_rate",
            outcome = "store_error",
            store_error = %store_error
        );
        return Ok(refused(StatusCode::SERVICE_UNAVAILABLE));
    }
    let rate = attorney_rates::NewAttorneyRate {
        person_id: input.person_id,
        project_id: input.project_id,
        hourly_rate_cents: input.hourly_rate_cents,
        effective_on: input.effective_on,
    };
    match attorney_rates::set_hourly_rate(&pg, rate).await {
        Ok(row) => Ok((StatusCode::CREATED, Json(row)).into_response()),
        Err(e) => Ok(fee_error(&e)),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlatFeeInput {
    fee_cents: i64,
}

/// `GET /app/api/projects/{id}/flat-fee` — the matter's current agreed flat
/// fee, if one is set.
pub(crate) async fn flat_fee(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match postgres("flat_fee", Some(project_id)).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match attorney_rates::flat_fee_cents(&pg, project_id).await {
        Ok(fee) => Ok(Json(serde_json::json!({ "fee_cents": fee })).into_response()),
        Err(e) => Ok(fee_error(&e)),
    }
}

/// `POST /app/api/projects/{id}/flat-fee` — set the matter's current agreed
/// flat fee, replacing any prior figure. Captured invoices are unaffected.
pub(crate) async fn set_flat_fee(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
    Json(input): Json<FlatFeeInput>,
) -> Result<Response, ApiError> {
    ensure_firm_side(&state, session.person_id, session.role, project_id).await?;
    let pg = match prepared_store(&state, "set_flat_fee", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match attorney_rates::set_flat_fee(&pg, project_id, input.fee_cents).await {
        Ok(fee) => Ok(Json(serde_json::json!({ "fee_cents": fee })).into_response()),
        Err(e) => Ok(fee_error(&e)),
    }
}
