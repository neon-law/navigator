//! Native-form write doors for the matter dashboard's Billing tab — the
//! browser half of the time-tracking and invoice-generation commands, beside
//! the JSON APIs in [`crate::time_tracking_api`] and
//! [`crate::project_billing_api`].
//!
//! The dashboard is a native-form surface: the timer, log-time, and
//! generate-invoice controls POST here and the handlers redirect (303) back
//! to the Billing tab's path, with an `?error=` flash when a command is refused and
//! a `?flash=` success toast naming what happened (the invoice just raised).
//! The authorization, mirror preparation, rate resolution, and store
//! commands are the JSON doors' own — both doors call the same
//! `prepared_store`, `snapshot_rate`, and `timer_on_matter` seams, so the
//! two entry points cannot drift. The flash copy refuses in the user's
//! vocabulary — never a store variant name, and never a number the store
//! should have computed (minutes, elapsed time, and invoice totals are the
//! store's computations, never the wire's).
use axum::{
    extract::{Form, Path, State},
    response::{IntoResponse, Redirect, Response},
    Extension,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::admin::{encode_query_value, not_found_response};
use crate::session::SessionData;

/// Back to the Billing tab the control lives on, success or refusal alike.
/// The success toast rides `?flash=`; a refusal rides `?error=` — two
/// channels, because one names what happened and the other what to do next.
fn back_to_billing(code: &str, flash: Option<&str>, error: Option<&str>) -> Response {
    let mut url = format!("/app/projects/{code}/billing");
    if let Some(flash) = flash {
        url.push_str("&flash=");
        url.push_str(&encode_query_value(flash));
    }
    if let Some(error) = error {
        url.push_str("&error=");
        url.push_str(&encode_query_value(error));
    }
    Redirect::to(&url).into_response()
}

/// The door's own admission, verbatim from the stage door: a firm-side
/// viewer of this matter or the non-disclosing 404.
async fn firm_side_viewer(
    surreal: &store::surreal::SurrealDb,
    session: &SessionData,
    project_id: Uuid,
) -> bool {
    if !crate::admin::is_lawyer_tier(Some(session)) {
        return false;
    }
    matches!(
        store::access::matter_viewer(surreal, session.person_id, session.role, project_id)
            .await
            .ok()
            .flatten(),
        Some(viewer) if viewer.is_firm_side()
    )
}

/// The log-time control on the Billing tab: what was done, when, for how
/// long, and whether it bills. The attorney is the reader — the form carries
/// no attorney picker, so the rate that freezes onto the entry is the
/// reader's own.
#[derive(Deserialize)]
pub(crate) struct LogTimeForm {
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) performed_on: String,
    #[serde(default)]
    pub(crate) minutes: String,
    #[serde(default)]
    pub(crate) billable: Option<String>,
}

/// `POST /app/projects/{code}/time-entries` — log time by hand. The rate is
/// never taken from the wire: it is resolved from the reader's fee structure
/// for the performed date and frozen onto the row, exactly as the JSON door
/// does.
pub(crate) async fn log_time(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<LogTimeForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&surreal, &session, project.id).await {
        return not_found_response();
    }

    let description = input.description.trim().to_string();
    let Ok(minutes) = input.minutes.trim().parse::<i32>() else {
        return back_to_billing(
            &code,
            None,
            Some("Capture the time in whole minutes — for example, 90."),
        );
    };
    let Ok(performed_on) = chrono::NaiveDate::parse_from_str(input.performed_on.trim(), "%Y-%m-%d")
    else {
        return back_to_billing(&code, None, Some("Give the date the work was performed."));
    };
    let billable = input.billable.as_deref() == Some("on");

    let Ok(pg) =
        crate::time_tracking_api::prepared_store(&surreal, "create_entry", project.id).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("The time record is unavailable right now — try again shortly."),
        );
    };
    let Ok(rate_cents_per_hour) =
        crate::time_tracking_api::snapshot_rate(&pg, person_id, project.id, performed_on, billable)
            .await
    else {
        return back_to_billing(
            &code,
            None,
            Some("Billable time needs a rate for this matter — set your hourly rate first."),
        );
    };
    let entry = store::time_entries::NewTimeEntry {
        attorney_id: person_id,
        performed_on,
        minutes,
        description,
        billable,
        rate_cents_per_hour,
        source: store::time_entries::TimeEntrySource::Manual,
    };
    match store::time_entries::create(&pg, project.id, person_id, &entry).await {
        Ok(_) => back_to_billing(&code, Some("Time logged."), None),
        Err(e) => back_to_billing(&code, None, Some(&flash_for_entry(&e))),
    }
}

/// The start-timer control in the matter header: an optional note. The
/// durable row exists the moment this returns; the header's ticking display
/// is only an animation of it.
#[derive(Deserialize)]
pub(crate) struct TimerStartForm {
    #[serde(default)]
    pub(crate) note: String,
}

/// `POST /app/projects/{code}/timer/start` — start the calling attorney's
/// durable Timer on this matter. At most one Timer runs per attorney,
/// firm-wide: starting a second is refused with the way to resolve it.
pub(crate) async fn start_timer(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<TimerStartForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&surreal, &session, project.id).await {
        return not_found_response();
    }
    let Ok(pg) =
        crate::time_tracking_api::prepared_store(&surreal, "start_timer", project.id).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("The timer record is unavailable right now — try again shortly."),
        );
    };
    let note = input.note.trim().to_string();
    match store::timers::start_timer(
        &pg,
        person_id,
        project.id,
        (!note.is_empty()).then_some(note),
    )
    .await
    {
        Ok(_) => back_to_billing(
            &code,
            Some("Timer running — it keeps counting while you work."),
            None,
        ),
        Err(e) => back_to_billing(&code, None, Some(&flash_for_timer(&e))),
    }
}

/// The stop-timer control: what the captured entry should say, and whether
/// it bills. The minutes arrive from the Timer row, never the form.
#[derive(Deserialize)]
pub(crate) struct TimerStopForm {
    #[serde(default)]
    pub(crate) description: String,
    #[serde(default)]
    pub(crate) billable: Option<String>,
}

/// `POST /app/projects/{code}/timer/stop` — stop the calling attorney's open
/// Timer on this matter and capture its elapsed minutes as a Time Entry in
/// one transaction. The rate is resolved for the date the Timer started and
/// frozen onto the entry; the minutes are the store's own computation.
pub(crate) async fn stop_timer(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<TimerStopForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&surreal, &session, project.id).await {
        return not_found_response();
    }
    let Ok(pg) = crate::time_tracking_api::prepared_store(&surreal, "stop_timer", project.id).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("The timer record is unavailable right now — try again shortly."),
        );
    };
    let running = match store::timers::running_for_person(&pg, person_id).await {
        Ok(running) => running,
        Err(e) => return back_to_billing(&code, None, Some(&flash_for_timer(&e))),
    };
    let Some(timer) = crate::time_tracking_api::timer_on_matter(running, project.id) else {
        return back_to_billing(&code, None, Some("The timer is no longer running."));
    };
    let billable = input.billable.as_deref() == Some("on");
    let Ok(rate_cents_per_hour) = crate::time_tracking_api::snapshot_rate(
        &pg,
        person_id,
        project.id,
        timer.started_at.date_naive(),
        billable,
    )
    .await
    else {
        return back_to_billing(
            &code,
            None,
            Some("Billable time needs a rate for this matter — set your hourly rate first."),
        );
    };
    let entry = store::time_entries::NewTimeEntry {
        attorney_id: person_id,
        // stop_timer stamps these from the Timer itself: the elapsed minutes,
        // the Timer source, and the date the Timer started.
        performed_on: timer.started_at.date_naive(),
        minutes: 0,
        description: input.description.trim().to_string(),
        billable,
        rate_cents_per_hour,
        source: store::time_entries::TimeEntrySource::Timer,
    };
    match store::timers::stop_timer(&pg, timer.id, person_id, entry).await {
        Ok(captured) => back_to_billing(
            &code,
            Some(&format!(
                "Captured {} minutes of time — review it in the time table.",
                captured.minutes
            )),
            None,
        ),
        Err(e) => back_to_billing(&code, None, Some(&flash_for_timer(&e))),
    }
}

/// `POST /app/projects/{code}/timer/discard` — discard the calling
/// attorney's open Timer on this matter without recording anything.
/// Deliberate, owner-only, and transactional, exactly like the JSON door.
pub(crate) async fn discard_timer(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&surreal, &session, project.id).await {
        return not_found_response();
    }
    let Ok(pg) = crate::time_tracking_api::postgres("discard_timer", Some(project.id)).await else {
        return back_to_billing(
            &code,
            None,
            Some("The timer record is unavailable right now — try again shortly."),
        );
    };
    let running = match store::timers::running_for_person(&pg, person_id).await {
        Ok(running) => running,
        Err(e) => return back_to_billing(&code, None, Some(&flash_for_timer(&e))),
    };
    let Some(timer) = crate::time_tracking_api::timer_on_matter(running, project.id) else {
        return back_to_billing(&code, None, Some("The timer is no longer running."));
    };
    match store::timers::discard_timer(&pg, timer.id, person_id).await {
        Ok(()) => back_to_billing(&code, Some("Timer discarded — nothing was recorded."), None),
        Err(e) => back_to_billing(&code, None, Some(&flash_for_timer(&e))),
    }
}

/// The confirm form's one field: present when the raise was confirmed.
#[derive(Deserialize)]
pub(crate) struct GenerateInvoiceForm {
    #[serde(default)]
    pub(crate) confirm: Option<String>,
}

/// `POST /app/projects/{code}/invoices` — the generate-invoice action's two
/// halves. Without `confirm`, the POST only opens the compose preview (the
/// confirm step renders at `?confirm=1`); with it, the composed invoice is
/// raised through the provider seam. Raising is externally visible — Xero is
/// the books — so the confirm step is not optional.
pub(crate) async fn generate_invoice(
    State(admin): State<crate::admin::AdminState>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<GenerateInvoiceForm>,
) -> Response {
    let Some(session) = session else {
        return not_found_response();
    };
    if session.viewing_as_dri.is_some() {
        return not_found_response();
    }
    let Some(person_id) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&admin.surreal, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&admin.surreal, &session, project.id).await {
        return not_found_response();
    }
    // The first POST is the action's start: nothing provider-bound has run,
    // and the compose preview is a read rendered at `?confirm=1`.
    if input.confirm.is_none() {
        return Redirect::to(&format!("/app/projects/{code}/billing?confirm=1"))
            .into_response();
    }
    let Ok(pg) = crate::time_tracking_api::postgres("generate_invoice", Some(project.id)).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("The billing record is unavailable right now — try again shortly."),
        );
    };
    let now: DateTime<Utc> = Utc::now();
    match crate::project_billing_api::generate_invoice(
        &admin.surreal,
        &pg,
        admin.billing_provider.as_ref(),
        project.id,
        person_id,
        now,
    )
    .await
    {
        Ok(receipt) => back_to_billing(&code, Some(&raise_confirmation(&receipt)), None),
        Err(e) => back_to_billing(&code, None, Some(&flash_for_invoice(&e, project.id))),
    }
}

/// The success toast: the invoice named by its reference — the client's copy
/// of the books — and the work it consumed. The amount itself renders in the
/// invoice table the redirect lands on, formatted once, by the tab.
fn raise_confirmation(receipt: &crate::project_billing_api::InvoiceReceipt) -> String {
    if receipt.flat_fee {
        format!(
            "Invoice {} raised in Xero — the matter's flat fee, with {} expenses billed alongside it.",
            receipt.reference, receipt.consumed_costs
        )
    } else {
        format!(
            "Invoice {} raised in Xero — {} time entries and {} expenses billed.",
            receipt.reference, receipt.consumed_time_entries, receipt.consumed_costs
        )
    }
}

/// The store's typed refusals, rendered as flash copy in the user's
/// vocabulary — never a variant name. The failure path preserves the
/// command's own guarantee: nothing flipped to Invoiced, and a retry raises
/// the same invoice rather than a second one.
fn flash_for_invoice(
    error: &crate::project_billing_api::InvoiceGenerationError,
    project_id: Uuid,
) -> String {
    use crate::project_billing_api::InvoiceGenerationError;
    match error {
        InvoiceGenerationError::BillingUnavailable => {
            tracing::warn!(
                event = "invoice_generation_billing_unavailable",
                outcome = "unavailable",
                project_id = %project_id
            );
            "Invoicing is unavailable right now — try again shortly.".to_string()
        }
        InvoiceGenerationError::NothingToInvoice => {
            "There is nothing unbilled to invoice on this matter yet.".to_string()
        }
        InvoiceGenerationError::FlatFeeAlreadyBilled
        | InvoiceGenerationError::Batch(
            store::billing_batches::BillingBatchError::FlatFeeAlreadyBilled,
        ) => {
            "The matter's agreed flat fee is already billed — amend the fee for additional work."
                .to_string()
        }
        InvoiceGenerationError::ReconciliationRequired
        | InvoiceGenerationError::InvoiceNotBillable => {
            "The last invoice attempt needs reconciliation before another raise — ask support to reconcile it.".to_string()
        }
        InvoiceGenerationError::CurrencyMismatch(currency) => format!(
            "An unbilled expense is in {currency}; matter invoices are raised in USD."
        ),
        InvoiceGenerationError::NoBilledParty => {
            "The matter names no client to bill — add the client's participation first.".to_string()
        }
        InvoiceGenerationError::FutureDatedCost(date) => format!(
            "An expense is dated {date}, after today's invoice — resolve its date first."
        ),
        InvoiceGenerationError::Billing(_) => {
            // The provider refused: the caller retries the same command. The
            // failure names the upstream in the log, never in the flash.
            tracing::warn!(
                event = "invoice_generation_provider_failed",
                outcome = "provider_error",
                project_id = %project_id
            );
            "Xero refused the invoice — nothing was raised. Try again in a moment; a retry raises the same invoice.".to_string()
        }
        _ => "The billing record is unavailable right now — try again shortly.".to_string(),
    }
}

/// The time-entry store's refusals, in the log-time form's vocabulary.
fn flash_for_entry(error: &store::time_entries::TimeEntryError) -> String {
    use store::time_entries::TimeEntryError;
    match error {
        TimeEntryError::Invalid("minutes") => {
            "Capture the time in whole minutes, more than zero — for example, 90.".to_string()
        }
        TimeEntryError::Invalid(_) => {
            "Check the entry fields and submit the form again.".to_string()
        }
        TimeEntryError::NotParticipant => {
            "Only a participant on this matter logs time on it.".to_string()
        }
        TimeEntryError::Conflict => {
            "That work is already invoiced — it cannot be logged again.".to_string()
        }
        TimeEntryError::NotFound => {
            "The time record is gone — review the table and try again.".to_string()
        }
        TimeEntryError::Database(_) | TimeEntryError::Postgres(_) => {
            "The time record is unavailable right now — try again shortly.".to_string()
        }
    }
}

/// The timer store's refusals, in the timer widget's vocabulary.
fn flash_for_timer(error: &store::timers::TimerError) -> String {
    use store::timers::TimerError;
    match error {
        TimerError::Conflict => {
            "You already have a timer running — stop or discard it before starting another."
                .to_string()
        }
        TimerError::NotParticipant => {
            "Only a participant on this matter runs a timer on it.".to_string()
        }
        TimerError::NotOwner | TimerError::NotFound => {
            "The timer is no longer running.".to_string()
        }
        TimerError::Invalid(_) => "Check the timer fields and try again.".to_string(),
        TimerError::Entry(entry) => flash_for_entry(entry),
        TimerError::Database(_) | TimerError::Postgres(_) => {
            "The timer record is unavailable right now — try again shortly.".to_string()
        }
    }
}
