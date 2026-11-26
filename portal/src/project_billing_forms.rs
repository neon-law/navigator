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

#[derive(Deserialize)]
pub(crate) struct AnnotationReview {
    decision: store::billing_annotations::Decision,
    note: String,
}

pub(crate) async fn review_annotation(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path((code, id)): Path<(String, Uuid)>,
    Form(input): Form<AnnotationReview>,
) -> Response {
    use store::time_entries::TimeEntryError;

    let Some(session) = session.filter(|s| s.viewing_as_dri.is_none()) else {
        return not_found_response();
    };
    let Some(actor) = session.person_id else {
        return not_found_response();
    };
    let Some(project) = store::projects::find_by_code(&db, &code)
        .await
        .ok()
        .flatten()
    else {
        return not_found_response();
    };
    if !firm_side_viewer(&db, &session, project.id).await {
        return not_found_response();
    }
    let Ok(pg) =
        crate::time_tracking_api::prepared_store(&db, "review_annotation", project.id).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("Billing is unavailable. Try again shortly."),
        );
    };
    let result =
        store::billing_annotations::review(&pg, project.id, actor, id, input.decision, &input.note)
            .await;
    let error = match result {
        Ok(()) => return back_to_billing(&code, Some("Billing review recorded."), None),
        Err(TimeEntryError::Conflict) => "This annotation is no longer awaiting review.".to_owned(),
        Err(TimeEntryError::Invalid("contingency agreement has no hourly fee")) => {
            "The contingency agreement has no hourly fee. Record the time at no charge or deny it."
                .to_owned()
        }
        Err(TimeEntryError::Invalid("missing hourly rate")) => {
            "Set the hourly rate for the date of service before approving billable time.".to_owned()
        }
        Err(error) => flash_for_entry(&error),
    };
    back_to_billing(&code, None, Some(&error))
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

#[derive(Deserialize)]
pub(crate) struct SetRateForm {
    hourly_rate: String,
    effective_on: String,
}

/// Parse a decimal-dollar field into integer cents without floating point.
fn hourly_rate_cents(raw: &str) -> Option<i32> {
    let raw = raw.trim();
    let (dollars, cents) = raw.split_once('.').unwrap_or((raw, ""));
    if dollars.is_empty()
        || !dollars.bytes().all(|byte| byte.is_ascii_digit())
        || !cents.bytes().all(|byte| byte.is_ascii_digit())
        || cents.len() > 2
    {
        return None;
    }
    let dollars = dollars.parse::<i64>().ok()?;
    let cents = format!("{cents:0<2}").parse::<i64>().ok()?;
    i32::try_from(dollars.checked_mul(100)?.checked_add(cents)?).ok()
}

/// `POST /app/projects/{code}/rates` — add a matter-scoped Attorney Rate for
/// the current lawyer; the date and amount are validated before insertion.
pub(crate) async fn set_matter_rate(
    State(surreal): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    Path(code): Path<String>,
    Form(input): Form<SetRateForm>,
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
    let Some(hourly_rate_cents) = hourly_rate_cents(&input.hourly_rate) else {
        return back_to_billing(
            &code,
            None,
            Some("Enter a non-negative hourly rate in dollars."),
        );
    };
    let Ok(effective_on) = chrono::NaiveDate::parse_from_str(input.effective_on.trim(), "%Y-%m-%d")
    else {
        return back_to_billing(
            &code,
            None,
            Some("Give the date this hourly rate takes effect."),
        );
    };
    let Ok(pg) =
        crate::time_tracking_api::prepared_store(&surreal, "set_matter_rate", project.id).await
    else {
        return back_to_billing(
            &code,
            None,
            Some("Billing is unavailable. Try again shortly."),
        );
    };
    match store::attorney_rates::set_hourly_rate(
        &pg,
        store::attorney_rates::NewAttorneyRate {
            person_id,
            project_id: Some(project.id),
            hourly_rate_cents,
            effective_on,
        },
    )
    .await
    {
        Ok(_) => back_to_billing(&code, Some("Matter rate added to history."), None),
        Err(store::attorney_rates::FeeStructureError::Conflict) => back_to_billing(
            &code,
            None,
            Some("A rate already exists for that date. Add a rate with a new effective date."),
        ),
        Err(_) => back_to_billing(
            &code,
            None,
            Some("Billing is unavailable. Try again shortly."),
        ),
    }
}

#[derive(Deserialize)]
pub(crate) struct DailyBillingForm {
    project: String,
    description: String,
    hours: Option<String>,
    minutes: Option<String>,
    billable: Option<String>,
    performed_on: chrono::NaiveDate,
}

impl DailyBillingForm {
    fn minutes_and_billable(&self) -> Result<(i32, bool), &'static str> {
        match (self.minutes.as_deref(), self.hours.as_deref()) {
            (Some(raw), None) => {
                let minutes = raw
                    .trim()
                    .parse::<i32>()
                    .ok()
                    .filter(|n| (1..=1440).contains(n))
                    .ok_or("Enter whole minutes from 1 to 1440.")?;
                Ok((minutes, self.billable.as_deref() == Some("on")))
            }
            (None, Some(hours)) => daily_billing_minutes(hours)
                .map(|minutes| (minutes, true))
                .ok_or("Enter hours in 0.1 increments, from 0.1 to 24."),
            _ => Err("Submit one duration in whole minutes."),
        }
    }
}

/// The hours form submits billable time in exact tenths.
fn daily_billing_minutes(hours: &str) -> Option<i32> {
    let hours = hours.trim();
    let (whole, fraction) = hours.split_once('.').unwrap_or((hours, ""));
    let fraction = fraction.trim_end_matches('0');
    if !whole.bytes().all(|b| b.is_ascii_digit()) || fraction.len() > 1 {
        return None;
    }
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<i32>().ok()?
    };
    let tenth = match fraction.as_bytes() {
        [] => 0,
        [digit] if digit.is_ascii_digit() => i32::from(digit - b'0'),
        _ => return None,
    };
    let tenths = whole.checked_mul(10)?.checked_add(tenth)?;
    (1..=240).contains(&tenths).then(|| tenths * 6)
}

pub(crate) async fn daily_billing(
    State(db): State<store::surreal::SurrealDb>,
    session: Option<Extension<SessionData>>,
    brand: Option<Extension<views::brand::BrandKey>>,
    Form(form): Form<DailyBillingForm>,
) -> Response {
    let Some(Extension(actor)) = session.filter(|s| s.viewing_as_dri.is_none()) else {
        return not_found_response();
    };
    let brand = brand.map(|Extension(b)| b).unwrap_or_default();
    let Ok(projects) = store::access::visible_lawyer_projects_for_brand(
        &db,
        actor.person_id,
        actor.role,
        brand.as_str(),
    )
    .await
    else {
        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(project) = projects.into_iter().find(|p| p.code == form.project) else {
        return not_found_response();
    };
    let path = format!(
        "{}?day={}",
        webapp::lawyer_dashboard::billing::PATH,
        form.performed_on
    );
    let Some(person_id) = actor.person_id else {
        return not_found_response();
    };
    let result = match form.minutes_and_billable() {
        Ok((minutes, billable)) => {
            record_manual_time(
                &db,
                project.id,
                person_id,
                form.performed_on,
                minutes,
                form.description,
                billable,
            )
            .await
        }
        Err(error) => Err(error.to_owned()),
    };
    let (key, message) = match &result {
        Ok(()) => ("flash", "Time logged."),
        Err(error) => ("error", error.as_str()),
    };
    Redirect::to(&format!("{path}&{key}={}", encode_query_value(message))).into_response()
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
    match record_manual_time(
        &surreal,
        project.id,
        person_id,
        performed_on,
        minutes,
        input.description,
        input.billable.as_deref() == Some("on"),
    )
    .await
    {
        Ok(()) => back_to_billing(&code, Some("Time logged."), None),
        Err(error) => back_to_billing(&code, None, Some(&error)),
    }
}

async fn record_manual_time(
    db: &store::surreal::SurrealDb,
    project_id: Uuid,
    person_id: Uuid,
    performed_on: chrono::NaiveDate,
    minutes: i32,
    description: String,
    billable: bool,
) -> Result<(), String> {
    let pg = crate::time_tracking_api::prepared_store(db, "create_entry", project_id)
        .await
        .map_err(|_| "The time record is unavailable right now — try again shortly.")?;
    let rate_cents_per_hour =
        crate::time_tracking_api::snapshot_rate(&pg, person_id, project_id, performed_on, billable)
            .await
            .map_err(|_| {
                "Billable time needs a rate for this matter — set your hourly rate first."
            })?;
    let entry = store::time_entries::NewTimeEntry {
        attorney_id: person_id,
        performed_on,
        minutes,
        description,
        billable,
        rate_cents_per_hour,
        source: store::time_entries::TimeEntrySource::Manual,
    };
    store::time_entries::create(&pg, project_id, person_id, &entry)
        .await
        .map(|_| ())
        .map_err(|e| flash_for_entry(&e))
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
            "The draft is ready for review in the state trust account for this matter.".to_string()
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
        generate_invoice, hourly_rate_cents, log_time, raise_confirmation, start_timer, stop_timer,
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

    #[test]
    fn hourly_rate_form_uses_exact_integer_cents() {
        assert_eq!(hourly_rate_cents("500"), Some(50_000));
        assert_eq!(hourly_rate_cents("100.5"), Some(10_050));
        assert_eq!(hourly_rate_cents("0"), Some(0));
        assert_eq!(hourly_rate_cents("-1"), None);
        assert_eq!(hourly_rate_cents("1.001"), None);
        assert_eq!(hourly_rate_cents("21474836.48"), None);
    }

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
            "The draft is ready for review in the state trust account for this matter."
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
    async fn daily_billing_decodes_minutes_and_keeps_hours_submissions_billable() {
        use axum::extract::FromRequest;
        for (fields, expected) in [
            ("hours=0.1", Some((6, true))),
            ("minutes=4", Some((4, false))),
            ("minutes=4&billable=on", Some((4, true))),
            ("minutes=1440", Some((1440, false))),
            ("hours=0.1&minutes=4", None),
            ("minutes=1.5", None),
            ("minutes=0", None),
            ("minutes=1441", None),
            ("", None),
        ] {
            let body =
                format!("project=sample&description=Review&performed_on=2001-11-15&{fields}");
            let request = axum::http::Request::post("/app/lawyer/billing")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(axum::body::Body::from(body))
                .unwrap();
            let Form(form) = Form::<super::DailyBillingForm>::from_request(request, &())
                .await
                .unwrap();
            assert_eq!(form.minutes_and_billable().ok(), expected, "{fields}");
        }
    }

    #[test]
    fn daily_billing_converts_exact_tenths_to_minutes() {
        for (hours, minutes) in [
            (".1", 6),
            ("0.50", 30),
            ("0.6", 36),
            ("1", 60),
            ("24.0", 1440),
        ] {
            assert_eq!(super::daily_billing_minutes(hours), Some(minutes));
        }
        for hours in [
            "",
            "0",
            "-0.1",
            "0.11",
            "0.01",
            "24.1",
            "1e2",
            "1..2",
            "9999999999",
        ] {
            assert_eq!(super::daily_billing_minutes(hours), None);
        }
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
#[cfg(test)]
mod end_to_end_tests {
    use super::*;
    use crate::session::DriView;
    use axum::http::{header::LOCATION, StatusCode};
    use chrono::NaiveDate;
    use store::participation::{AddParticipantCommand, DriActor, DriRequest};
    use store::persons::{NewPerson, Role};
    use store::projects::DriSide;

    fn location(response: &Response) -> String {
        response
            .headers()
            .get(LOCATION)
            .expect("a redirect carries its Location")
            .to_str()
            .expect("an ASCII Location")
            .to_string()
    }

    fn log_time_form(minutes: &str, performed_on: &str, billable: bool) -> Form<LogTimeForm> {
        Form(LogTimeForm {
            description: "Drafted the motion to compel".into(),
            performed_on: performed_on.into(),
            minutes: minutes.into(),
            billable: billable.then(|| "on".into()),
        })
    }

    /// Plant the people-authority rows the matter and its participation
    /// reference. Under people authority PostgreSQL is authoritative and
    /// the mirror never copies these rows, so the fixture writes the
    /// matter's client entity and its lawyer directly.
    async fn seed_pg_identity(
        pg: &store::postgres::PostgresDb,
        entity_id: Uuid,
        lawyer: &store::persons::Person,
    ) {
        let catalog = Uuid::now_v7();
        sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,$2)")
            .bind(catalog)
            .bind(format!("Synthetic {catalog}"))
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,$2)")
            .bind(catalog)
            .bind(format!("Synthetic {catalog}"))
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,$2,$3,$4)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(entity_id)
        .bind("Synthetic Entity")
        .bind(catalog)
        .bind(catalog)
        .execute(pg.pool())
        .await
        .unwrap();
        // People authority may already have copied the lawyer's row via
        // participation mirroring; the fixture only guarantees presence.
        sqlx::query(
            "INSERT INTO person(id,name,email) VALUES ($1,$2,$3) ON CONFLICT (id) DO NOTHING",
        )
        .bind(lawyer.id)
        .bind(&lawyer.name)
        .bind(&lawyer.email)
        .execute(pg.pool())
        .await
        .unwrap();
    }

    /// A throwaway pool for direct SQL the store seam does not expose —
    /// the child process's fixture database, read back from its env.
    async fn raw_test_pool() -> sqlx::PgPool {
        use sqlx::postgres::{PgConnectOptions, PgSslMode};
        sqlx::PgPool::connect_with(
            PgConnectOptions::new()
                .host(&std::env::var("NAVIGATOR_POSTGRES_HOST").expect("fixture host"))
                .port(
                    std::env::var("NAVIGATOR_POSTGRES_PORT")
                        .expect("fixture port")
                        .parse()
                        .expect("numeric port"),
                )
                .username(&std::env::var("NAVIGATOR_POSTGRES_USER").expect("fixture user"))
                .password(&std::env::var("NAVIGATOR_POSTGRES_PASSWORD").expect("fixture password"))
                .database(&std::env::var("NAVIGATOR_POSTGRES_DATABASE").expect("fixture database"))
                .ssl_mode(PgSslMode::Disable),
        )
        .await
        .expect("connect to the fixture database")
    }

    /// A lawyer joined to a fresh matter, with the session the doors admit.
    async fn lawyer_on_seeded_matter(
        surreal: &store::surreal::SurrealDb,
    ) -> (
        store::projects::Project,
        store::persons::Person,
        SessionData,
    ) {
        // `projects::create` validates brands against live rows, so the
        // compiled brand catalog has to exist before any matter is seeded.
        store::surreal::test_support::seed_compiled_brands(surreal).await;
        let project =
            store::test_support::seed_project(surreal, "Synthetic billing forms matter").await;
        let lawyer = store::persons::create(
            surreal,
            &NewPerson::with_role(
                "Synthetic billing lawyer",
                format!("billing-forms-{}@example.com", Uuid::new_v4()),
                Role::Lawyer,
            ),
        )
        .await
        .expect("seed the billing lawyer");
        store::participation::add_participant(
            surreal,
            &AddParticipantCommand {
                project_id: project.id,
                person_id: lawyer.id,
                dri: DriRequest::Unchanged,
                actor: DriActor::System,
            },
        )
        .await
        .expect("join the lawyer to the matter");
        let session = SessionData {
            person_id: Some(lawyer.id),
            ..SessionData::fresh("synthetic-billing@example.com", Role::Lawyer)
        };
        (project, lawyer, session)
    }

    // ---- pure helpers ----

    #[test]
    fn back_to_billing_rides_two_channels_and_encodes_both() {
        let response = back_to_billing("acme", None, None);
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(location(&response), "/app/projects/acme/billing");

        let response = back_to_billing("acme", Some("Time logged."), None);
        assert_eq!(
            location(&response),
            "/app/projects/acme/billing?flash=Time%20logged."
        );

        let response = back_to_billing("acme", None, Some("Capture the time in whole minutes."));
        assert_eq!(
            location(&response),
            "/app/projects/acme/billing?error=Capture%20the%20time%20in%20whole%20minutes."
        );

        let response = back_to_billing("acme", Some("Time & time again?"), Some("#hash"));
        assert_eq!(
            location(&response),
            "/app/projects/acme/billing?flash=Time%20%26%20time%20again%3F&error=%23hash"
        );
    }

    #[test]
    fn the_raise_confirmation_names_the_destination_and_the_work() {
        use crate::project_billing_api::InvoiceReceipt;

        let receipt = InvoiceReceipt {
            xero_invoice_id: Some("stub-invoice-1".into()),
            stripe_invoice_id: None,
            stripe_iolta: None,
            reference: "INV-22".into(),
            currency: "USD".into(),
            total_cents: 225_000,
            line_items: Vec::new(),
            consumed_time_entries: 3,
            consumed_costs: 2,
            flat_fee: false,
        };
        assert_eq!(
            raise_confirmation(&receipt),
            "Invoice INV-22 raised in Xero — 3 time entries and 2 expenses billed."
        );

        let mut flat_fee_receipt = receipt.clone();
        flat_fee_receipt.flat_fee = true;
        assert_eq!(
            raise_confirmation(&flat_fee_receipt),
            "Invoice INV-22 raised in Xero — the matter's flat fee, with 2 expenses billed alongside it."
        );

        let mut iolta_receipt = receipt.clone();
        iolta_receipt.stripe_iolta = Some(true);
        assert_eq!(
            raise_confirmation(&iolta_receipt),
            "The draft is ready for review in the state trust account for this matter."
        );
        iolta_receipt.stripe_iolta = Some(false);
        assert_eq!(
            raise_confirmation(&iolta_receipt),
            "Stripe draft created for lawyer review. This is not IOLTA billing."
        );
    }

    #[test]
    fn every_invoice_refusal_lands_in_lawyer_copy_never_in_variant_names() {
        use crate::project_billing_api::InvoiceGenerationError;

        let flash = |error: &InvoiceGenerationError| flash_for_invoice(error, Uuid::new_v4());

        assert_eq!(
            flash(&InvoiceGenerationError::BillingUnavailable),
            "Invoicing is unavailable right now — try again shortly."
        );
        assert_eq!(
            flash(&InvoiceGenerationError::StripeUnavailable),
            "Stripe billing is unavailable for this Firm and jurisdiction."
        );
        assert_eq!(
            flash(&InvoiceGenerationError::NothingToInvoice),
            "There is nothing unbilled to invoice on this matter yet."
        );
        assert_eq!(
            flash(&InvoiceGenerationError::PreviewChanged),
            "The compose changed since the preview — review it again before raising."
        );
        assert_eq!(
            flash(&InvoiceGenerationError::FlatFeeAlreadyBilled),
            "The matter's agreed flat fee is already billed — amend the fee for additional work."
        );
        // The two hold-still refusals share one copy: the caller's remedy is
        // the same either way.
        assert_eq!(
            flash(&InvoiceGenerationError::ReconciliationRequired),
            flash(&InvoiceGenerationError::InvoiceNotBillable)
        );
        assert!(flash(&InvoiceGenerationError::ReconciliationRequired)
            .starts_with("The last invoice attempt needs reconciliation"));
        assert_eq!(
            flash(&InvoiceGenerationError::CurrencyMismatch("EUR".into())),
            "An unbilled expense is in EUR; matter invoices are raised in USD."
        );
        assert_eq!(
            flash(&InvoiceGenerationError::NoBilledParty),
            "The matter names no client to bill — add the client's participation first."
        );
        let dated = flash(&InvoiceGenerationError::FutureDatedCost(
            NaiveDate::from_ymd_opt(2026, 12, 1).expect("a date"),
        ));
        assert_eq!(
            dated,
            "An expense is dated 2026-12-01, after today's invoice — resolve its date first."
        );
        assert!(flash(&InvoiceGenerationError::Billing(
            billing::BillingError::Provider("synthetic provider fault".into())
        ))
        .starts_with("Xero refused the invoice"));
        // The fallthrough refusal keeps the transport failure out of the flash.
        assert_eq!(
            flash(&InvoiceGenerationError::NoMatter),
            "The billing record is unavailable right now — try again shortly."
        );
    }

    #[test]
    fn timer_and_entry_refusals_speak_the_form_s_vocabulary() {
        use store::time_entries::TimeEntryError;
        use store::timers::TimerError;

        // The timer's entry wrap shares the log-time form's copy.
        assert_eq!(
            flash_for_timer(&TimerError::Entry(TimeEntryError::Invalid("minutes"))),
            "Capture the time in whole minutes, more than zero — for example, 90."
        );
        assert_eq!(
            flash_for_entry(&TimeEntryError::NotParticipant),
            "Only a participant on this matter logs time on it."
        );
        assert_eq!(
            flash_for_entry(&TimeEntryError::Conflict),
            "That work is already invoiced — it cannot be logged again."
        );
        assert_eq!(
            flash_for_entry(&TimeEntryError::NotFound),
            "The time record is gone — review the table and try again."
        );
        assert_eq!(
            flash_for_timer(&TimerError::Conflict),
            "You already have a timer running — stop or discard it before starting another."
        );
        assert_eq!(
            flash_for_timer(&TimerError::NotParticipant),
            "Only a participant on this matter runs a timer on it."
        );
        assert_eq!(
            flash_for_timer(&TimerError::NotOwner),
            "The timer is no longer running."
        );
        assert_eq!(
            flash_for_timer(&TimerError::Invalid("note")),
            "Check the timer fields and try again."
        );
    }

    // ---- admission, no PostgreSQL needed ----

    #[tokio::test]
    async fn the_form_doors_are_a_non_disclosing_404_without_admission() {
        let surreal = crate::test_support::embedded_surreal().await;

        for status in [
            log_time(
                State(surreal.clone()),
                None,
                Path("acme".into()),
                log_time_form("90", "2026-11-17", true),
            )
            .await
            .status(),
            start_timer(
                State(surreal.clone()),
                None,
                Path("acme".into()),
                Form(TimerStartForm {
                    note: String::new(),
                }),
            )
            .await
            .status(),
            stop_timer(
                State(surreal.clone()),
                None,
                Path("acme".into()),
                Form(TimerStopForm {
                    description: String::new(),
                    billable: None,
                }),
            )
            .await
            .status(),
            discard_timer(State(surreal.clone()), None, Path("acme".into()))
                .await
                .status(),
        ] {
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        // A session with no person row behind it — the brief boot window —
        // is refused the same way, and never leaks that the matter exists.
        let anonymous = SessionData {
            person_id: None,
            ..SessionData::fresh("synthetic@example.com", Role::Lawyer)
        };
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(anonymous.clone())),
            Path("acme".into()),
            log_time_form("90", "2026-11-17", true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response = start_timer(
            State(surreal.clone()),
            Some(Extension(anonymous)),
            Path("acme".into()),
            Form(TimerStartForm {
                note: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Viewing as another person never opens the billing doors.
        let viewing = SessionData {
            viewing_as_dri: Some(DriView {
                actor_sub: "synthetic-actor".into(),
                actor_email: None,
                actor_person_id: None,
                target_name: "Synthetic target".into(),
                target_email: "target@example.com".into(),
            }),
            person_id: Some(Uuid::new_v4()),
            ..SessionData::fresh("synthetic@example.com", Role::Lawyer)
        };
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(viewing)),
            Path("acme".into()),
            log_time_form("90", "2026-11-17", true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // An unknown matter code reads the same refusal.
        let stranger = SessionData {
            person_id: Some(Uuid::new_v4()),
            ..SessionData::fresh("synthetic@example.com", Role::Lawyer)
        };
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(stranger)),
            Path("no-such-matter".into()),
            log_time_form("90", "2026-11-17", true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_client_tier_session_is_refused_the_billing_doors() {
        let surreal = crate::test_support::embedded_surreal().await;
        // `projects::create` validates brands against live rows; seed the
        // compiled brand catalog before any matter.
        store::surreal::test_support::seed_compiled_brands(&surreal).await;
        let project =
            store::test_support::seed_project(&surreal, "Synthetic client tier matter").await;
        let client = store::persons::create(
            &surreal,
            &NewPerson::with_role(
                "Synthetic client",
                format!("billing-client-{}@example.com", Uuid::new_v4()),
                Role::Client,
            ),
        )
        .await
        .expect("seed the client");
        store::participation::add_participant(
            &surreal,
            &AddParticipantCommand {
                project_id: project.id,
                person_id: client.id,
                dri: DriRequest::Designate(DriSide::Client),
                actor: DriActor::System,
            },
        )
        .await
        .expect("join the client to the matter");
        let session = SessionData {
            person_id: Some(client.id),
            ..SessionData::fresh("synthetic-client@example.com", Role::Client)
        };

        assert_eq!(
            log_time(
                State(surreal.clone()),
                Some(Extension(session.clone())),
                Path(project.code.clone()),
                log_time_form("90", "2026-11-17", true)
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            start_timer(
                State(surreal.clone()),
                Some(Extension(session.clone())),
                Path(project.code.clone()),
                Form(TimerStartForm {
                    note: String::new(),
                }),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            stop_timer(
                State(surreal.clone()),
                Some(Extension(session.clone())),
                Path(project.code.clone()),
                Form(TimerStopForm {
                    description: String::new(),
                    billable: None,
                }),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            discard_timer(
                State(surreal.clone()),
                Some(Extension(session)),
                Path(project.code.clone()),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_malformed_minutes_or_date_field_refuses_in_plain_lawyer_copy() {
        let surreal = crate::test_support::embedded_surreal().await;
        let (project, _lawyer, session) = lawyer_on_seeded_matter(&surreal).await;

        let response = log_time(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            log_time_form("an hour and a half", "2026-11-17", true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = location(&response);
        assert!(
            url.contains("error=Capture%20the%20time%20in%20whole%20minutes"),
            "unexpected redirect: {url}"
        );
        assert!(!url.contains("flash="), "no success flash on a refusal");

        let response = log_time(
            State(surreal.clone()),
            Some(Extension(session)),
            Path(project.code.clone()),
            log_time_form("90", "tomorrow", true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = location(&response);
        assert!(
            url.contains("error=Give%20the%20date%20the%20work%20was%20performed"),
            "unexpected redirect: {url}"
        );
    }

    // ---- end to end, against PostgreSQL ----

    /// The billing doors against PostgreSQL: log time with and without a fee
    /// structure, run a timer from start to stop to discard, and raise an
    /// invoice through the stub provider — the actions an attorney performs
    /// on the matter page every day.
    #[expect(
        clippy::too_many_lines,
        reason = "one workflow, four doors: log time, drive the timer, raise the invoice"
    )]
    async fn billing_form_doors_end_to_end() {
        // PostgreSQL holds the mirrors the fixture writes, so migrate first.
        let pg = store::postgres::PostgresDb::from_env().await.unwrap();
        pg.migrate_embedded(None).await.unwrap();
        // `test_support::mem_surreal` would count as an embedded-engine
        // call site (cli/tests/embedded_surreal_ratchet.rs), so use the
        // portal helper instead; the seeding helper registers the brands.
        let surreal = crate::test_support::embedded_surreal().await;
        let (project, lawyer, session) = lawyer_on_seeded_matter(&surreal).await;

        // People authority keeps each entity and person row
        // PostgreSQL-side — the mirror never copies them — so the
        // fixture plants the matter's client entity and its lawyer
        // directly, then publishes the matter and the lawyer's
        // participation, which the doors' journal reads join.
        seed_pg_identity(&pg, project.entity_id, &lawyer).await;
        store::postgres_mirror::mirror(
            &surreal,
            &pg,
            &store::postgres_mirror::MirrorKey::Project(project.id),
        )
        .await
        .expect("mirror the matter into PostgreSQL");
        for role in store::projects::participations_for_project(&surreal, project.id)
            .await
            .expect("read the lawyer's participation")
        {
            store::postgres_mirror::mirror(
                &surreal,
                &pg,
                &store::postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await
            .expect("mirror the participation into PostgreSQL");
        }

        // The lawyer's fee structure prices billable time: 250 cents an
        // hour from two days ago, so any performed-on date resolves.
        let today = Utc::now().date_naive();
        store::attorney_rates::set_hourly_rate(
            &pg,
            store::attorney_rates::NewAttorneyRate {
                person_id: lawyer.id,
                project_id: Some(project.id),
                hourly_rate_cents: 25_000,
                effective_on: today - chrono::Duration::days(2),
            },
        )
        .await
        .expect("set the billing lawyer's hourly rate");

        // Billable time freezes the attorney's rate onto the row.
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            log_time_form("90", &today.to_string(), true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            location(&response).ends_with("flash=Time%20logged."),
            "unexpected redirect: {}",
            location(&response)
        );
        let entries = store::time_entries::list_for_project(&pg, project.id)
            .await
            .expect("read the logged time");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].attorney_id, lawyer.id);
        assert_eq!(entries[0].minutes, 90);
        assert_eq!(entries[0].rate_cents_per_hour, 25_000);
        assert!(entries[0].billable);
        assert_eq!(entries[0].source.as_str(), "manual");
        assert_eq!(entries[0].billing_status.as_str(), "unbilled");

        // No-charge time logs without any fee structure: the rate snapshot
        // is zero and no attorney rate row is consulted.
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            log_time_form("30", &today.to_string(), false),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).ends_with("flash=Time%20logged."));
        let entries = store::time_entries::list_for_project(&pg, project.id)
            .await
            .expect("read the logged time");
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|entry| !entry.billable));
        assert!(
            entries.iter().all(|entry| entry.rate_cents_per_hour >= 0),
            "the rate snapshot never goes negative"
        );

        // A second lawyer without a fee structure is refused billable time,
        // in the form's own words, and nothing is logged.
        let rateless = store::persons::create(
            &surreal,
            &NewPerson::with_role(
                "Synthetic rateless lawyer",
                format!("billing-rateless-{}@example.com", Uuid::new_v4()),
                Role::Lawyer,
            ),
        )
        .await
        .expect("seed the rateless lawyer");
        store::participation::add_participant(
            &surreal,
            &AddParticipantCommand {
                project_id: project.id,
                person_id: rateless.id,
                dri: DriRequest::Unchanged,
                actor: DriActor::System,
            },
        )
        .await
        .expect("join the rateless lawyer to the matter");
        let rateless_session = SessionData {
            person_id: Some(rateless.id),
            ..SessionData::fresh("synthetic-rateless@example.com", Role::Lawyer)
        };
        let response = log_time(
            State(surreal.clone()),
            Some(Extension(rateless_session)),
            Path(project.code.clone()),
            log_time_form("15", &today.to_string(), true),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = location(&response);
        assert!(
            url.contains("error=") && url.contains("hourly%20rate"),
            "unexpected redirect: {url}"
        );
        assert_eq!(
            store::time_entries::list_for_project(&pg, project.id)
                .await
                .expect("read the logged time")
                .len(),
            2,
            "the refused entry was not written"
        );

        // Start the timer.
        let response = start_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(TimerStartForm {
                note: "Called the client".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).contains("flash=Timer%20running"));
        let timer = store::timers::running_for_person(&pg, lawyer.id)
            .await
            .expect("read the running timer")
            .expect("the timer is running");
        assert_eq!(timer.project_id, project.id);

        // A second start is refused in the widget's own words.
        let response = start_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(TimerStartForm {
                note: String::new(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).contains("error=You%20already%20have%20a%20timer%20running"));

        // Stop it, backdated 90 minutes so the captured entry has real
        // minutes: the entry rides the timer's performed-on date and the
        // same rate snapshot.
        sqlx::query(
            "UPDATE timer SET started_at = started_at - interval '90 minutes' WHERE id = $1",
        )
        .bind(timer.id)
        .execute(&raw_test_pool().await)
        .await
        .expect("backdate the timer");
        let response = stop_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(TimerStopForm {
                description: "Called the client".into(),
                billable: Some("on".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = location(&response);
        assert!(
            url.contains("flash=Captured%2090%20minutes"),
            "unexpected redirect: {url}"
        );
        let entries = store::time_entries::list_for_project(&pg, project.id)
            .await
            .expect("read the captured time");
        assert_eq!(entries.len(), 3);
        let captured = entries
            .iter()
            .find(|entry| entry.source == store::time_entries::TimeEntrySource::Timer)
            .expect("the timer capture");
        assert_eq!(captured.minutes, 90);
        assert_eq!(captured.rate_cents_per_hour, 25_000);
        assert!(captured.billable);
        assert_eq!(captured.billing_status.as_str(), "unbilled");
        assert_eq!(captured.description, "Called the client");

        // Stopping again — no timer left — names the real state.
        let response = stop_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(TimerStopForm {
                description: String::new(),
                billable: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).contains("error=The%20timer%20is%20no%20longer%20running."));

        // A discarded timer records nothing.
        let response = start_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(TimerStartForm {
                note: "A false start".into(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let response = discard_timer(
            State(surreal.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(location(&response).contains("flash=Timer%20discarded"));
        assert_eq!(
            store::time_entries::list_for_project(&pg, project.id)
                .await
                .expect("read the logged time")
                .len(),
            3,
            "a discard never writes time"
        );

        // Raise the invoice: the first POST opens the compose preview, and
        // the confirmed POST raises it through the stub provider.
        // The invoice command needs a provider that admits invoices: the
        // shared `app_state` stub refuses by default, while a simulated-
        // matters boot selects the admitting stub (hosting.rs).
        let mut app = crate::test_support::app_state(surreal.clone()).await;
        app.billing_provider =
            std::sync::Arc::new(crate::billing::StubBillingProvider::for_simulated_matters());
        let admin = blog_admin_state(app);
        let response = generate_invoice(
            State(admin.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(GenerateInvoiceForm {
                confirm: None,
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            location(&response).ends_with("billing?confirm=1"),
            "the unconfirmed POST opens the compose preview: {}",
            location(&response)
        );

        // The client-side participation is the billed party.
        let client = store::persons::create(
            &surreal,
            &NewPerson::with_role(
                "Synthetic billed client",
                format!("billing-party-{}@example.com", Uuid::new_v4()),
                Role::Client,
            ),
        )
        .await
        .expect("seed the billed client");
        store::participation::add_participant(
            &surreal,
            &AddParticipantCommand {
                project_id: project.id,
                person_id: client.id,
                dri: DriRequest::Designate(DriSide::Client),
                actor: DriActor::System,
            },
        )
        .await
        .expect("join the billed client to the matter");

        let response = generate_invoice(
            State(admin.clone()),
            Some(Extension(session.clone())),
            Path(project.code.clone()),
            Form(GenerateInvoiceForm {
                confirm: Some("on".into()),
                digest: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let url = location(&response);
        assert!(
            url.contains("flash=") && url.contains("raised%20in%20Xero"),
            "unexpected redirect: {url}"
        );
        // Every consumed entry flipped to invoiced under the provider id.
        let entries = store::time_entries::list_for_project(&pg, project.id)
            .await
            .expect("read the invoiced time");
        assert!(
            entries
                .iter()
                .all(|entry| entry.billing_status.as_str() == "invoiced"),
            "the raise consumed every unbilled entry: {entries:?}"
        );
    }

    /// `blog_admin_state`'s shape: an `AdminState` assembled field-by-field
    /// from the shared `AppState`, so the invoice command runs against the
    /// same stub billing provider the production boot selects.
    fn blog_admin_state(app: crate::AppState) -> crate::admin::AdminState {
        crate::admin::AdminState {
            surreal: app.surreal,
            workflow_runtime: app.workflow_runtime,
            signature_provider: app.signature_provider,
            retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
            questionnaire_runtime: app.questionnaire_runtime,
            storage: app.storage,
            assets_storage: app.assets_storage,
            forms_registry: app.forms_registry,
            email: app.email,
            billing_provider: app.billing_provider,
            contract_reviewer: app.contract_reviewer,
            bootstrap_owner_email: app.bootstrap_owner_email,
            on_call_lawyer_email: app.on_call_lawyer_email,
            bootstrap_company: crate::admin::bootstrap_company_from_env(),
            sessions: app.sessions,
            secure_cookies: false,
            attachment_scanner: app.attachment_scanner,
            runtime_kms: app.runtime_kms,
            twilio_registry: app.twilio_registry,
        }
    }

    #[tokio::test]
    async fn the_billing_form_doors_log_time_and_drive_the_timer_end_to_end() {
        if std::env::var("NAV_BILLING_FORM_CHILD").as_deref() == Ok("1") {
            Box::pin(billing_form_doors_end_to_end()).await;
            return;
        }
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
                "PostgreSQL fixture required"
            );
            return;
        };
        let admin = sqlx::PgPool::connect_with(
            sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port.parse().unwrap())
                .username("postgres")
                .password("synthetic")
                .database("smoke")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable),
        )
        .await
        .unwrap();
        let database = format!("billing_forms_{}", Uuid::new_v4().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
            .execute(&admin)
            .await
            .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "project_billing_forms::tests::the_billing_form_doors_log_time_and_drive_the_timer_end_to_end",
                "--nocapture",
            ])
            .env("NAV_BILLING_FORM_CHILD", "1")
            .env("NAVIGATOR_RELEASE_TAG", "26.11.17")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", &port)
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_DATABASE", &database)
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .status()
            .unwrap();
        sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
            .bind(&database)
            .execute(&admin)
            .await
            .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
            .execute(&admin)
            .await
            .unwrap();
        admin.close().await;
        assert!(
            status.success(),
            "the billing form end-to-end child failed — run with --nocapture"
        );
    }
}
