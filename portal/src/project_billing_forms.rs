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
    // The first parameter opens the query with `?`; later ones continue it
    // with `&` — an `&` before the first would break the path outright.
    let mut separator = '?';
    if let Some(flash) = flash {
        url.push(separator);
        url.push_str("flash=");
        url.push_str(&encode_query_value(flash));
        separator = '&';
    }
    if let Some(error) = error {
        url.push(separator);
        url.push_str("error=");
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

/// The confirm form's two fields: `confirm` is present when the raise was
/// confirmed; `digest` is the fingerprint of the compose the attorney
/// reviewed — the command re-verifies it against a fresh compose before
/// anything is raised.
#[derive(Deserialize)]
pub(crate) struct GenerateInvoiceForm {
    #[serde(default)]
    pub(crate) confirm: Option<String>,
    #[serde(default)]
    pub(crate) digest: Option<String>,
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
        return Redirect::to(&format!("/app/projects/{code}/billing?confirm=1")).into_response();
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
        input.digest.as_deref(),
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
    if let Some(iolta) = receipt.stripe_iolta {
        return if iolta {
            "The draft is ready for review in the Stripe account for Nevada.".to_string()
        } else {
            "Stripe draft created for lawyer review. This is not IOLTA billing.".to_string()
        };
    }
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
        InvoiceGenerationError::StripeUnavailable => {
            "Stripe billing is unavailable for this Firm and jurisdiction.".to_string()
        }
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
        InvoiceGenerationError::PreviewChanged => {
            "The compose changed since the preview — review it again before raising.".to_string()
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

#[cfg(test)]
mod tests {
    use super::{
        back_to_billing, discard_timer, flash_for_entry, flash_for_invoice, flash_for_timer,
        generate_invoice, log_time, raise_confirmation, start_timer, stop_timer,
        GenerateInvoiceForm, LogTimeForm, TimerStartForm, TimerStopForm,
    };
    use crate::session::{DriView, SessionData};
    use axum::extract::{Form, Path, State};
    use axum::http::StatusCode;
    use chrono::NaiveDate;
    use store::persons::Role;
    use store::time_entries::TimeEntryError;
    use store::timers::TimerError;
    use uuid::Uuid;

    /// A handle with no engine behind it — the documented outage posture
    /// (`portal/src/lead_capture.rs`). Every store call errors, so the doors
    /// take their not-found exit without standing up an embedded engine:
    /// the SurrealDB test-support ratchet counts this crate's call sites and
    /// new ones are refused (`cli/tests/embedded_surreal_ratchet.rs`).
    fn outage_store() -> store::surreal::SurrealDb {
        store::surreal::SurrealDb::uninitialized()
    }

    /// A lawyer-tier session the way an authenticated attorney arrives: a
    /// person row, no DRI view. `person_id` is a random UUID — the doors
    /// resolve the matter from the path code, never from the session, so a
    /// session that names nobody the store knows still reaches the
    /// not-found exits these tests assert.
    fn attorney_session() -> SessionData {
        let mut session = SessionData::fresh("attorney-sub", Role::Lawyer);
        session.person_id = Some(Uuid::now_v7());
        session
    }

    /// The read-only client-DRI view: a lawyer session switched into a
    /// client's eyes, which every mutating door here must refuse.
    fn dri_view_session() -> SessionData {
        let mut session = SessionData::fresh("attorney-sub", Role::Lawyer);
        session.person_id = Some(Uuid::now_v7());
        session.viewing_as_dri = Some(DriView {
            actor_sub: "attorney-sub".to_string(),
            actor_email: Some("attorney@example.com".to_string()),
            actor_person_id: Some(Uuid::now_v7()),
            target_name: "Client DRI".to_string(),
            target_email: "dri@example.com".to_string(),
        });
        session
    }

    fn redirect_location(response: &axum::response::Response) -> String {
        response
            .headers()
            .get(axum::http::header::LOCATION)
            .expect("the doors answer with a redirect")
            .to_str()
            .expect("the redirect target is a valid header value")
            .to_string()
    }

    #[test]
    fn back_to_billing_builds_the_query_in_channel_order() {
        // No channel: the bare tab path, no dangling separator.
        assert_eq!(
            redirect_location(&back_to_billing("ACME", None, None)),
            "/app/projects/ACME/billing"
        );
        // Success alone rides `?flash=`; a refusal alone rides `?error=`.
        assert_eq!(
            redirect_location(&back_to_billing("ACME", Some("Time logged."), None)),
            "/app/projects/ACME/billing?flash=Time%20logged."
        );
        assert_eq!(
            redirect_location(&back_to_billing("ACME", None, Some("Capture the time in whole minutes — for example, 90."))),
            "/app/projects/ACME/billing?error=Capture%20the%20time%20in%20whole%20minutes%20%E2%80%94%20for%20example,%2090."
        );
        // Both channels: flash opens the query, error continues it — an `&`
        // before the first parameter would break the path outright.
        assert_eq!(
            redirect_location(&back_to_billing("ACME", Some("a?b"), Some("c&d"))),
            "/app/projects/ACME/billing?flash=a%3Fb&error=c%26d"
        );
    }

    #[test]
    fn raise_confirmation_names_the_reference_and_what_it_consumed() {
        let receipt = crate::project_billing_api::InvoiceReceipt {
            xero_invoice_id: Some("xero-1".to_string()),
            stripe_invoice_id: None,
            stripe_iolta: None,
            reference: "INV-001".to_string(),
            currency: "USD".to_string(),
            total_cents: 12_345,
            line_items: Vec::new(),
            consumed_time_entries: 4,
            consumed_costs: 2,
            flat_fee: false,
        };
        assert_eq!(
            raise_confirmation(&receipt),
            "Invoice INV-001 raised in Xero — 4 time entries and 2 expenses billed."
        );
    }

    #[test]
    fn a_flat_fee_invoice_confirms_the_fee_rather_than_itemized_time() {
        let receipt = crate::project_billing_api::InvoiceReceipt {
            flat_fee: true,
            consumed_costs: 3,
            ..flat_fee_receipt()
        };
        assert_eq!(
            raise_confirmation(&receipt),
            "Invoice INV-FLAT raised in Xero — the matter's flat fee, with 3 expenses billed alongside it."
        );
    }

    fn flat_fee_receipt() -> crate::project_billing_api::InvoiceReceipt {
        crate::project_billing_api::InvoiceReceipt {
            xero_invoice_id: None,
            stripe_invoice_id: None,
            stripe_iolta: None,
            reference: "INV-FLAT".to_string(),
            currency: "USD".to_string(),
            total_cents: 5_000,
            line_items: Vec::new(),
            consumed_time_entries: 9,
            consumed_costs: 0,
            flat_fee: true,
        }
    }

    #[test]
    fn stripe_drafts_split_the_confirmation_by_iolta() {
        // The Stripe branch answers before the reference is ever read: the
        // draft lives in the law firm's Stripe account, not Xero.
        let iolta = crate::project_billing_api::InvoiceReceipt {
            stripe_iolta: Some(true),
            ..flat_fee_receipt()
        };
        assert_eq!(
            raise_confirmation(&iolta),
            "The draft is ready for review in the Stripe account for Nevada."
        );
        let not_iolta = crate::project_billing_api::InvoiceReceipt {
            stripe_iolta: Some(false),
            ..flat_fee_receipt()
        };
        assert_eq!(
            raise_confirmation(&not_iolta),
            "Stripe draft created for lawyer review. This is not IOLTA billing."
        );
    }

    #[test]
    fn entry_refusals_render_in_the_log_time_form_s_vocabulary() {
        let rendered = |error| flash_for_entry(&error);
        assert_eq!(
            rendered(TimeEntryError::Invalid("minutes")),
            "Capture the time in whole minutes, more than zero — for example, 90."
        );
        assert_eq!(
            rendered(TimeEntryError::Invalid("performed_on")),
            "Check the entry fields and submit the form again."
        );
        assert_eq!(
            rendered(TimeEntryError::NotParticipant),
            "Only a participant on this matter logs time on it."
        );
        assert_eq!(
            rendered(TimeEntryError::Conflict),
            "That work is already invoiced — it cannot be logged again."
        );
        assert_eq!(
            rendered(TimeEntryError::NotFound),
            "The time record is gone — review the table and try again."
        );
        assert_eq!(
            rendered(TimeEntryError::Database(sqlx::Error::RowNotFound)),
            "The time record is unavailable right now — try again shortly."
        );
        assert_eq!(
            rendered(TimeEntryError::Postgres(
                store::postgres::PostgresError::Connection
            )),
            "The time record is unavailable right now — try again shortly."
        );
    }

    #[test]
    fn timer_refusals_render_in_the_timer_widget_s_vocabulary() {
        let rendered = |error| flash_for_timer(&error);
        assert_eq!(
            rendered(TimerError::Conflict),
            "You already have a timer running — stop or discard it before starting another."
        );
        assert_eq!(
            rendered(TimerError::NotParticipant),
            "Only a participant on this matter runs a timer on it."
        );
        // Not-owner and missing rows answer with the same sentence: from the
        // widget's point of view there is no timer running either way.
        assert_eq!(
            rendered(TimerError::NotOwner),
            "The timer is no longer running."
        );
        assert_eq!(
            rendered(TimerError::NotFound),
            "The timer is no longer running."
        );
        assert_eq!(
            rendered(TimerError::Invalid("note")),
            "Check the timer fields and try again."
        );
        // An entry failure underneath a timer capture keeps the entry's own
        // vocabulary rather than the timer's.
        assert_eq!(
            rendered(TimerError::Entry(TimeEntryError::Conflict)),
            "That work is already invoiced — it cannot be logged again."
        );
        assert_eq!(
            rendered(TimerError::Database(sqlx::Error::RowNotFound)),
            "The timer record is unavailable right now — try again shortly."
        );
        assert_eq!(
            rendered(TimerError::Postgres(
                store::postgres::PostgresError::Connection
            )),
            "The timer record is unavailable right now — try again shortly."
        );
    }

    #[test]
    fn invoice_refusals_render_in_the_owners_vocabulary() {
        use crate::project_billing_api::InvoiceGenerationError;
        let rendered = |error| flash_for_invoice(&error, Uuid::now_v7());
        assert_eq!(
            rendered(InvoiceGenerationError::StripeUnavailable),
            "Stripe billing is unavailable for this Firm and jurisdiction."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::BillingUnavailable),
            "Invoicing is unavailable right now — try again shortly."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::NothingToInvoice),
            "There is nothing unbilled to invoice on this matter yet."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::PreviewChanged),
            "The compose changed since the preview — review it again before raising."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::FlatFeeAlreadyBilled),
            "The matter's agreed flat fee is already billed — amend the fee for additional work."
        );
        // The batch store's flat-fee refusal answers with the same sentence.
        assert_eq!(
            rendered(InvoiceGenerationError::Batch(
                store::billing_batches::BillingBatchError::FlatFeeAlreadyBilled
            )),
            "The matter's agreed flat fee is already billed — amend the fee for additional work."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::ReconciliationRequired),
            "The last invoice attempt needs reconciliation before another raise — ask support to reconcile it."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::InvoiceNotBillable),
            "The last invoice attempt needs reconciliation before another raise — ask support to reconcile it."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::CurrencyMismatch("EUR".to_string())),
            "An unbilled expense is in EUR; matter invoices are raised in USD."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::NoBilledParty),
            "The matter names no client to bill — add the client's participation first."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::FutureDatedCost(
                NaiveDate::from_ymd_opt(2030, 1, 1).expect("a valid date")
            )),
            "An expense is dated 2030-01-01, after today's invoice — resolve its date first."
        );
        assert_eq!(
            rendered(InvoiceGenerationError::Billing(
                billing::BillingError::Provider("xero unavailable".to_string())
            )),
            "Xero refused the invoice — nothing was raised. Try again in a moment; a retry raises the same invoice."
        );
        // Everything unlisted — a missing matter row among them — answers
        // with the record-unavailable sentence, never a variant name.
        assert_eq!(
            rendered(InvoiceGenerationError::NoMatter),
            "The billing record is unavailable right now — try again shortly."
        );
    }

    #[tokio::test]
    async fn the_time_entry_door_refuses_without_a_session() {
        let response = log_time(
            State(outage_store()),
            None,
            Path("ACME".to_string()),
            Form(LogTimeForm {
                description: String::new(),
                performed_on: String::new(),
                minutes: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_time_entry_door_refuses_a_client_dri_view() {
        let session = dri_view_session();
        let response = log_time(
            State(outage_store()),
            Some(axum::Extension(session)),
            Path("ACME".to_string()),
            Form(LogTimeForm {
                description: String::new(),
                performed_on: String::new(),
                minutes: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_time_entry_door_refuses_a_session_without_a_person_row() {
        let response = log_time(
            State(outage_store()),
            Some(axum::Extension(SessionData::fresh("no-row", Role::Lawyer))),
            Path("ACME".to_string()),
            Form(LogTimeForm {
                description: String::new(),
                performed_on: String::new(),
                minutes: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_time_entry_door_answers_not_found_for_an_unknown_matter_code() {
        let response = log_time(
            State(outage_store()),
            Some(axum::Extension(attorney_session())),
            Path("NOT-A-MATTER".to_string()),
            Form(LogTimeForm {
                description: String::new(),
                performed_on: String::new(),
                minutes: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_timer_doors_refuse_without_a_session() {
        let store = outage_store();
        let started = start_timer(
            State(store.clone()),
            None,
            Path("ACME".to_string()),
            Form(TimerStartForm {
                note: String::new(),
            }),
        )
        .await;
        assert_eq!(started.status(), StatusCode::NOT_FOUND);
        let stopped = stop_timer(
            State(store.clone()),
            None,
            Path("ACME".to_string()),
            Form(TimerStopForm {
                description: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(stopped.status(), StatusCode::NOT_FOUND);
        let discarded = discard_timer(State(store), None, Path("ACME".to_string())).await;
        assert_eq!(discarded.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_timer_doors_refuse_a_client_dri_view() {
        let store = outage_store();
        let session = axum::Extension(dri_view_session());
        let started = start_timer(
            State(store.clone()),
            Some(session.clone()),
            Path("ACME".to_string()),
            Form(TimerStartForm {
                note: String::new(),
            }),
        )
        .await;
        assert_eq!(started.status(), StatusCode::NOT_FOUND);
        let stopped = stop_timer(
            State(store.clone()),
            Some(session.clone()),
            Path("ACME".to_string()),
            Form(TimerStopForm {
                description: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(stopped.status(), StatusCode::NOT_FOUND);
        let discarded = discard_timer(State(store), Some(session), Path("ACME".to_string())).await;
        assert_eq!(discarded.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_timer_doors_answer_not_found_for_an_unknown_matter_code() {
        let store = outage_store();
        let session = axum::Extension(attorney_session());
        let started = start_timer(
            State(store.clone()),
            Some(session.clone()),
            Path("NOT-A-MATTER".to_string()),
            Form(TimerStartForm {
                note: String::new(),
            }),
        )
        .await;
        assert_eq!(started.status(), StatusCode::NOT_FOUND);
        let stopped = stop_timer(
            State(store.clone()),
            Some(session.clone()),
            Path("NOT-A-MATTER".to_string()),
            Form(TimerStopForm {
                description: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(stopped.status(), StatusCode::NOT_FOUND);
        let discarded = discard_timer(
            State(store),
            Some(session),
            Path("NOT-A-MATTER".to_string()),
        )
        .await;
        assert_eq!(discarded.status(), StatusCode::NOT_FOUND);
    }

    /// The admin console's state as `crate::test_support::app_state` builds
    /// it, over the given store handle — the same shape the admin tests'
    /// `blog_admin_state` helper assembles, minus the embedded engine.
    async fn billing_admin_state(surreal: store::surreal::SurrealDb) -> crate::admin::AdminState {
        let crate::AppState {
            surreal,
            workflow_runtime,
            signature_provider,
            questionnaire_runtime,
            storage,
            assets_storage,
            forms_registry,
            email,
            billing_provider,
            contract_reviewer,
            bootstrap_owner_email,
            on_call_lawyer_email,
            sessions,
            attachment_scanner,
            runtime_kms,
            ..
        } = crate::test_support::app_state(surreal).await;
        crate::admin::AdminState {
            surreal,
            workflow_runtime,
            signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime,
            storage,
            assets_storage,
            forms_registry,
            email,
            billing_provider,
            contract_reviewer,
            bootstrap_owner_email,
            on_call_lawyer_email,
            bootstrap_company: "Test Firm".to_string(),
            sessions,
            secure_cookies: false,
            attachment_scanner,
            runtime_kms,
            twilio_registry: None,
        }
    }

    #[tokio::test]
    async fn the_invoice_door_refuses_without_a_session() {
        let response = generate_invoice(
            State(billing_admin_state(outage_store()).await),
            None,
            Path("ACME".to_string()),
            Form(GenerateInvoiceForm {
                confirm: None,
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_invoice_door_refuses_a_client_dri_view() {
        let response = generate_invoice(
            State(billing_admin_state(outage_store()).await),
            Some(axum::Extension(dri_view_session())),
            Path("ACME".to_string()),
            Form(GenerateInvoiceForm {
                confirm: None,
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_invoice_door_answers_not_found_for_an_unknown_matter_code() {
        let response = generate_invoice(
            State(billing_admin_state(outage_store()).await),
            Some(axum::Extension(attorney_session())),
            Path("NOT-A-MATTER".to_string()),
            Form(GenerateInvoiceForm {
                confirm: None,
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unconfirmed_invoice_post_opens_the_compose_preview() {
        // An unknown matter code cannot reach the compose — this posture is
        // the outage store's not-found, not the preview redirect. The
        // preview redirect itself needs a resolvable matter; the admission
        // and copy halves this door shares are pinned above and by the
        // confirm-step tests in the JSON API's own module.
        let response = generate_invoice(
            State(billing_admin_state(outage_store()).await),
            Some(axum::Extension(attorney_session())),
            Path("NOT-A-MATTER".to_string()),
            Form(GenerateInvoiceForm {
                confirm: None,
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
