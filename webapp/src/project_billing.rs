//! The Billing tab's rows: a matter's time entries, invoices, expenses, and
//! fee structures, plus the matter header's running-timer widget.
//!
//! The dashboard's read seam for the time-tracking and billing stores
//! (`store::time_entries`, `store::timers`, `store::attorney_rates`,
//! `store::xero_invoices`, `store::project_costs`), beside the write doors in
//! the portal's time-tracking JSON API and the tab's native form handlers.
//! Reached only through `store::access::matter_viewer` — a participation row
//! is required of every tier (ENG-81), and Billing is a firm-side surface, so
//! a client-side participant, a clerk, an unassigned lawyer, Owner and Admin
//! included, all get the non-disclosing `404`.
//!
//! Rows are plain scalars — no `store` type crosses to the wasm build — and
//! money renders server-formatted from integer cents, never floats. The
//! invoice list reads the local Xero mirror, never Xero live.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Empty, Field, FormCard, Heading};
use crate::portal_project_detail::TrustView;

#[cfg(feature = "server")]
pub(crate) async fn read_claims(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    project: &store::projects::Project,
    viewer: store::access::MatterViewer,
    role: store::persons::Role,
    person: Option<uuid::Uuid>,
) -> Result<(bool, Vec<PlanClaimRow>), ServerFnError> {
    use crate::lawyer_project_detail::server_error;
    use crate::portal_project_detail::format_usd;
    use store::firm_capability::FirmCapability;

    let Some(firm) = project.firm_id.filter(|_| viewer.is_firm_side()) else {
        return Ok((false, Vec::new()));
    };
    if !FirmCapability::ViewDirectory
        .resolve(application_store, role, person, firm)
        .await
        .map_err(server_error)?
        .is_allowed()
    {
        return Ok((false, Vec::new()));
    }
    let mut connection = pg.pool().acquire().await.map_err(server_error)?;
    Ok((
        project.legal_plan_id.is_some(),
        store::plan_claims::for_matter_billing(&mut connection, project, firm)
            .await
            .map_err(server_error)?
            .into_iter()
            .map(|claim| {
                let billing = claim.billing;
                PlanClaimRow {
                    id: billing.id,
                    plan: claim.plan_name,
                    case_number: claim.plan_case_number,
                    benefit: billing.benefit,
                    service_date: billing.performed_on.to_string(),
                    minutes: claim.attorney_minutes,
                    covered_minutes: claim.covered_minutes,
                    annotation_price: claim.annotation_rate.map(|rate| {
                        if rate == 0 {
                            "Verified fixed fee".to_owned()
                        } else {
                            format!("{} per hour", format_usd(i64::from(rate)))
                        }
                    }),
                    status: crate::sections::instruments::words(&billing.status),
                    projected: billing.projected_fee_cents.map(format_usd),
                    filed: billing
                        .filed_cents
                        .map(|cents| format_usd(i64::from(cents))),
                    outstanding: billing.approved_outstanding_cents.map(format_usd),
                    received: format_usd(billing.net_cash_received_cents),
                    time_entry_ids: claim.time_entry_ids,
                }
            })
            .collect(),
    ))
}

/// One time entry on the matter, for the tab's table.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TimeEntryRow {
    pub id: String,
    /// The attorney's display name, resolved from the person directory.
    pub attorney: String,
    /// ISO `yyyy-mm-dd` — the date the work was performed.
    pub performed_on: String,
    /// Whole minutes of work, 1..=1440.
    pub minutes: i32,
    pub description: String,
    pub billable: bool,
    /// The entry's priced amount, server-formatted from the snapshotted rate
    /// and minutes. Non-billable work renders unpriced (a dash), never $0.
    pub amount: Option<String>,
    /// The stored word: `timer` or `manual` (`store::time_entries::
    /// TimeEntrySource::as_str`) — where the row came from.
    pub source: String,
    /// The stored word: `unbilled`, `reserved`, `invoiced`, or `written_off`
    /// (`store::time_entries::BillingStatus::as_str`).
    pub billing_status: String,
}

/// A Legal Plan Claim snapshot, including the Time Entries used to measure its work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanClaimRow {
    id: uuid::Uuid,
    plan: String,
    case_number: String,
    benefit: String,
    service_date: String,
    minutes: Option<i32>,
    covered_minutes: Option<i32>,
    annotation_price: Option<String>,
    status: String,
    projected: Option<String>,
    filed: Option<String>,
    outstanding: Option<String>,
    received: String,
    time_entry_ids: Vec<uuid::Uuid>,
}

impl PlanClaimRow {
    fn coverage(&self) -> Option<String> {
        let cap = self.covered_minutes?;
        let used = self.minutes?.min(cap);
        let remaining = cap - used;
        Some(format!("{used} of {cap} used · {remaining} remaining"))
    }
}

/// Source and service date awaiting a lawyer's billing decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnotationRow {
    pub id: String,
    pub change_id: String,
    /// Repository URL for the staged commit, when the Project has a forge link.
    pub change_url: Option<String>,
    pub attorney: String,
    pub hours: String,
    pub date: String,
    #[serde(default)]
    pub minutes: i32,
}

/// One of the matter's invoices, read from the local Xero mirror — the same
/// rows the client page's client-safe `InvoiceView` renders. The reference links to the invoice on the lawyer's Billing tab. The status is the mirror's
/// own word (`AUTHORISED`, `PAID`, …).
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct InvoiceRow {
    #[serde(default)]
    pub xero_invoice_id: String,
    pub reference: String,
    pub status: String,
    /// Formatted total, e.g. `$3,333.00`.
    pub amount: String,
    /// Formatted paid-to-date, e.g. `$1,000.00`.
    pub paid: String,
    /// The date Xero raised the invoice, `yyyy-mm-dd`.
    pub issued_on: String,
    /// `true` once reconcile has seen the invoice paid in full.
    pub settled: bool,
}

/// One expense on the matter — a receipt-backed third-party charge
/// (`store::project_costs`), the same rows the project-costs JSON API serves.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ExpenseRow {
    /// ISO `yyyy-mm-dd` — the date the charge occurred.
    pub occurred_on: String,
    pub payee: String,
    pub description: String,
    /// Formatted amount, e.g. `$421.00`.
    pub amount: String,
    /// The stored words for the two statuses, presentation-cased —
    /// `Estimated`/`Confirmed` and `Unbilled`/`Reserved`/`Invoiced`/`Resolved`.
    pub amount_status: String,
    pub billing_status: String,
}

/// The fee structures that price this matter's new work: the viewer's own
/// resolved hourly rate and the matter's agreed flat fee, if any.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FeeStructureView {
    /// The viewer's hourly rate on this matter today — the matter override
    /// over the firm default, resolved for today's date. `None` when no rate
    /// is on file (new billable time cannot be priced) or the deployment has
    /// no PostgreSQL configured.
    pub hourly_rate: Option<String>,
    /// The viewer's firm default and this matter's override history.
    pub rate_history: Vec<RateHistoryRow>,
    /// The matter's current agreed flat fee, server-formatted. `None` when
    /// none is agreed — the matter bills hourly.
    pub flat_fee: Option<String>,
}

/// One dated Attorney Rate visible from this matter's Fee structure section.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateHistoryRow {
    pub scope: String,
    pub effective_on: String,
    pub hourly_rate: String,
}

/// The calling attorney's open Timer — the durable row, recovered from
/// `started_at` however the tab was left. One open timer per attorney,
/// firm-wide; the widget shows it wherever the attorney works.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TimerView {
    /// `started_at` as epoch milliseconds — the JS ticker's only input. The
    /// server-rendered label stands in until the script loads.
    pub started_at_epoch_millis: i64,
    /// Elapsed `h:mm:ss`, computed server-side at render from `started_at`.
    pub elapsed: String,
    /// The note the timer was started with, pre-filling the capture form.
    pub note: Option<String>,
    /// `true` when the timer runs on the matter being viewed — the only case
    /// the stop and discard controls render for. A timer running on another
    /// matter renders as a link to that matter: the doors are matter-scoped,
    /// so the capture happens there.
    pub on_this_matter: bool,
    /// The other matter's code and name, when the timer runs there.
    pub other_matter_code: Option<String>,
    pub other_matter_name: Option<String>,
}

/// `h:mm:ss` for a stopwatch display — hours unbounded, minutes and seconds
/// zero-padded. Pure, so the server's no-JS label and the ticker agree.
#[must_use]
pub fn format_elapsed(total_seconds: i64) -> String {
    let seconds = total_seconds.max(0);
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    format!("{hours}:{minutes:02}:{secs:02}")
}

/// One composed line of the generate-invoice preview: the work described the
/// way the invoice will say it, and its price.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PreviewLine {
    pub description: String,
    /// The line's priced amount, server-formatted from integer cents.
    pub amount: String,
}

/// The generate-invoice action's state, composed server-side as a preview of
/// the very composition the command runs — the same lines, in the same
/// order, priced by the same snapshot rules.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GenerateInvoiceView {
    /// `true` when raising would compose at least one line — the action is
    /// enabled only in this state, and the empty state explains what would
    /// make it true.
    pub ready: bool,
    /// The composed preview lines, in invoice order.
    pub lines: Vec<PreviewLine>,
    /// The composed total, server-formatted from integer cents.
    pub total: String,
    /// `true` when an earlier attempt's billing batch is still pending — the
    /// next raise resumes that exact batch rather than composing anew.
    pub resuming: bool,
    /// `true` when the matter bills its agreed flat fee instead of itemized
    /// time — the preview shows the fee as one line, not the covered hours.
    pub flat_fee: bool,
    /// When the compose is refused by rule rather than by emptiness, the
    /// sentence that says why — the flat fee already billed, a future-dated
    /// expense — rendered in place of the default empty-state copy.
    #[serde(default)]
    pub note: Option<String>,
    /// The fingerprint of THIS compose. The confirm step carries it back,
    /// and the command re-verifies it against a fresh compose before raising
    /// — work logged or edited since the preview re-opens the review.
    #[serde(default)]
    pub digest: String,
    /// Stripe draft destination, when this matter uses Stripe.
    #[serde(default)]
    pub stripe_iolta: Option<bool>,
    #[serde(default)]
    pub trust_state: Option<String>,
}

/// The human word and chip tone for an entry's billing status. `Reserved`
/// means the work is drawn into a pending billing batch — no invoice holds
/// it yet, and it can no longer be written off.
fn status_label(billing_status: &str) -> &'static str {
    match billing_status {
        "reserved" => "Reserved",
        "invoiced" => "Invoiced",
        "written_off" => "Written off",
        _ => "Unbilled",
    }
}

fn status_tone(billing_status: &str) -> BadgeTone {
    match billing_status {
        "reserved" => BadgeTone::Review,
        "invoiced" => BadgeTone::Ready,
        "written_off" => BadgeTone::Blocked,
        _ => BadgeTone::Next,
    }
}

/// Where an entry came from, in the tab's vocabulary — a bordered neutral
/// pill, the way the design system marks provenance.
fn source_label(source: &str) -> &'static str {
    match source {
        "timer" => "Timer",
        "jujutsu" => "Jujutsu",
        _ => "Manual",
    }
}

/// The human word and chip tone for an expense's billing status.
fn expense_billing_label(billing_status: &str) -> &'static str {
    match billing_status {
        "reserved" => "Reserved",
        "invoiced" => "Invoiced",
        "resolved" => "Resolved",
        _ => "Unbilled",
    }
}

fn expense_billing_tone(billing_status: &str) -> BadgeTone {
    match billing_status {
        "reserved" => BadgeTone::Review,
        "invoiced" => BadgeTone::Ready,
        "resolved" => BadgeTone::Blocked,
        _ => BadgeTone::Next,
    }
}

/// The human word and chip tone for an expense's amount status.
fn expense_amount_label(amount_status: &str) -> &'static str {
    match amount_status {
        "confirmed" => "Confirmed",
        _ => "Estimated",
    }
}

fn expense_amount_tone(amount_status: &str) -> BadgeTone {
    match amount_status {
        "confirmed" => BadgeTone::Source,
        _ => BadgeTone::Review,
    }
}

/// The matter header's running-timer widget, rendered on every tab. The
/// ticking display is the only JS: `timer-widget.js` animates the elapsed
/// label from `started_at`; the Postgres row is the source of truth and the
/// server-rendered label stands in before the script loads.
#[component]
pub fn MatterTimer(code: String, csrf: String, timer: TimerView) -> Element {
    let elapsed = timer.elapsed.clone();
    let note = timer.note.clone().unwrap_or_default();
    rsx! {
        div { class: "matter-timer",
            if timer.on_this_matter {
                span {
                    class: "matter-timer__elapsed",
                    "data-timer-started-at": "{timer.started_at_epoch_millis}",
                    role: "timer",
                    "aria-label": "Timer running for {elapsed}",
                    "{elapsed}"
                }
                span { class: "matter-timer__note", "timer running" }
                form {
                    class: "lawyer-detail__inline-form matter-timer-stop-form",
                    method: "post",
                    action: "/app/projects/{code}/timer/stop",
                    input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                    // Editable before capture: the stop creates the entry in
                    // one transaction, so the description and billable choice
                    // the attorney wants recorded ride the same submit — an
                    // empty description can no longer reach the store, and
                    // no-charge time stays possible.
                    {Field::text("Description", "description", note.clone())
                        .required()
                        .help("What the client's invoice will say.")
                        .render()}
                    {Field::checkbox("Billable", "billable", "on", true).render()}
                    button {
                        class: "nav-btn nav-btn--secondary matter-timer-stop",
                        r#type: "submit",
                        "Stop"
                    }
                }
                form {
                    class: "lawyer-detail__inline-form",
                    method: "post",
                    action: "/app/projects/{code}/timer/discard",
                    input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                    button {
                        class: "nav-btn nav-btn--secondary matter-timer-discard",
                        r#type: "submit",
                        "Discard"
                    }
                }
                document::Script { src: "/public/js/timer-widget.js", defer: true }
            } else {
                span {
                    class: "matter-timer__elapsed",
                    "data-timer-started-at": "{timer.started_at_epoch_millis}",
                    role: "timer",
                    "{elapsed}"
                }
                span { class: "matter-timer__note", "timer running on " }
                if let (Some(other_code), Some(other_name)) =
                    (timer.other_matter_code.clone(), timer.other_matter_name.clone())
                {
                    a {
                        class: "nav-link",
                        href: "/app/projects/{other_code}/billing",
                        "{other_name}"
                    }
                } else {
                    span { "another matter" }
                }
                document::Script { src: "/public/js/timer-widget.js", defer: true }
            }
        }
    }
}

/// The Billing tab: the matter's money, as `MyCase` and Clio render it —
/// time on the matter (with its durable timer), the fee structures that
/// price new work, the invoices Xero holds, the receipt-backed expenses,
/// and the client-trust position.
///
/// Every write is a native-form POST to the matter's time-tracking handlers,
/// which re-ask the same authorization this page was rendered under. The
/// invoice list reads the local Xero mirror, never Xero live.
#[component]
pub fn BillingTab(
    code: String,
    csrf: String,
    /// The `?error=` flash a refused write redirected back with.
    #[props(default)]
    error: Option<String>,
    /// `true` when the URL carries `?confirm=1` — the compose is shown and
    /// the confirm step replaces the action's first button.
    #[props(default)]
    confirming: bool,
    timer: Option<TimerView>,
    entries: Vec<TimeEntryRow>,
    #[props(default)] claims: Vec<PlanClaimRow>,
    #[props(default)] plan_linked: bool,
    #[props(default)] annotations: Vec<AnnotationRow>,
    fees: FeeStructureView,
    invoices: Vec<InvoiceRow>,
    expenses: Vec<ExpenseRow>,
    trust: TrustView,
    /// The generate-invoice action's composed state, computed server-side.
    generate: GenerateInvoiceView,
    /// Today as `yyyy-mm-dd`, pre-filling the log-time form's date.
    today: String,
) -> Element {
    let linked_claims: std::collections::HashMap<_, _> = claims
        .iter()
        .flat_map(|claim| {
            claim
                .time_entry_ids
                .iter()
                .map(move |entry| (entry.to_string(), claim.id))
        })
        .collect();
    let error_shown = error.is_some();
    let rate_error = error.as_deref().is_some_and(|message| {
        message.contains("hourly rate") || message.contains("rate for this matter")
    });
    let reviews: Vec<_> = annotations
        .iter()
        .map(|row| {
            let matches: Vec<_> = claims
                .iter()
                .filter(|claim| {
                    claim.annotation_price.is_some()
                        && claim.service_date == row.date
                        && claim.minutes.is_none_or(|minutes| minutes == row.minutes)
                })
                .collect();
            (row, matches)
        })
        .collect();
    rsx! {
        section { class: "lawyer-detail__section lawyer-detail__billing",
            h2 { "Billing" }
            if let Some(error) = error {
                p { class: "nav-form-error", role: "alert", "{error}"
                    if rate_error {
                        " " a { href: "#fee-structure", "Go to Fee structure" }
                    }
                }
            }
            section { id: "fee-structure", class: "lawyer-detail__subsection matter-billing-fees",
                h3 { "Fee structure" }
                if let Some(rate) = fees.hourly_rate.clone() {
                    p { class: "matter-billing-current-rate", "Effective today, your hourly rate is " strong { "{rate} per hour" } "." }
                } else if !plan_linked {
                    p { class: "nav-callout matter-billing-current-rate", role: "status",
                        "No hourly rate is on file for you on this matter — new billable time cannot be priced until one is recorded."
                    }
                }
                if plan_linked {
                    p { "Legal plan time uses the verified terms on the selected claim. Review the claim before approving time." }
                }
                p { class: "nav-muted",
                    "A matter override takes effect on its effective date. Otherwise, your firm-wide rate applies. New rates never change Time Entries already recorded."
                }
                form { class: "lawyer-detail__inline-form", method: "post", action: "/app/projects/{code}/rates",
                    input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                    label { class: "nav-label", "Hourly rate"
                        input { class: "nav-input", name: "hourly_rate", r#type: "number", min: "0", step: "0.01", required: true }
                    }
                    label { class: "nav-label", "Effective on"
                        input { class: "nav-input", name: "effective_on", r#type: "date", value: "{today}", required: true }
                    }
                    button { class: "nav-btn", r#type: "submit", "Add matter override" }
                }
                if !fees.rate_history.is_empty() {
                    h4 { "Rate history" }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table matter-billing-rate-history",
                            thead { tr {
                                th { scope: "col", "Scope" }
                                th { scope: "col", "Effective on" }
                                th { scope: "col", "Hourly rate" }
                            } }
                            tbody {
                                for row in fees.rate_history.iter() {
                                    tr {
                                        td { "{row.scope}" }
                                        td { "{row.effective_on}" }
                                        td { "{row.hourly_rate}" }
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(fee) = fees.flat_fee.clone() {
                    p { "Flat fee agreed: {fee} — billed as one line covering the matter's unbilled time." }
                } else {
                    p { class: "nav-muted", "No flat fee is recorded. The signed engagement governs hourly and contingent fees." }
                }
            }

            section { class: "lawyer-detail__subsection matter-billing-review",
                h3 { "Time awaiting review" }
                if annotations.is_empty() {
                    p { class: "nav-muted", "No Jujutsu time awaits review. Stage an annotated change with "
                        code { "navigator project jj stage-time" } "." }
                }
                for (row, matches) in &reviews {
                    form { key: "{row.id}", class: "nav-form",
                        method: "post", action: "/app/projects/{code}/billing-annotations/{row.id}/review",
                        input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                        fieldset {
                            legend { "{row.hours} hours · {row.attorney} · {row.date}" }
                            p { "Jujutsu change "
                                if let Some(url) = row.change_url.as_ref() {
                                    a { href: "{url}", target: "_blank", rel: "noopener noreferrer", code { "{row.change_id}" } }
                                } else {
                                    code { "{row.change_id}" }
                                }
                                " · review the patch before approving time."
                            }
                            label { class: "nav-label", "Invoice description or reason for denial"
                                input { class: "nav-input", name: "note", required: true, maxlength: "2000" }
                            }
                            if !matches.is_empty() {
                                label { class: "nav-label", "Legal plan claim"
                                    select { class: "nav-input", name: "claim_id",
                                        option { value: "", "Select a claim" }
                                        for claim in matches {
                                            option { value: "{claim.id}",
                                                "{claim.benefit} · {claim.annotation_price.as_deref().unwrap_or_default()}" }
                                        }
                                    }
                                }
                            }
                            div { class: "lawyer-detail__actions",
                                if !matches.is_empty() {
                                    button { class: "nav-btn", r#type: "submit", name: "decision", value: "plan_claim", "Approve to claim" }
                                }
                                button { class: "nav-btn", r#type: "submit", name: "decision", value: "bill",
                                    if plan_linked { "Approve for direct invoice" } else { "Approve billable time" }
                                }
                                button { class: "nav-btn nav-btn--secondary", r#type: "submit", name: "decision", value: "no_charge", "Record at no charge" }
                                button { class: "nav-btn nav-btn--secondary", r#type: "submit", name: "decision", value: "deny", "Deny" }
                            }
                        }
                    }
                }
            }
            section { class: "lawyer-detail__subsection matter-billing-time",
                h3 { "Time" }
                if timer.is_none() {
                    FormCard {
                        title: "Start the timer".to_string(),
                        action: "/app/projects/{code}/timer/start",
                        submit_label: "Start timer".to_string(),
                        heading: Heading::H2,
                        csrf_token: Some(csrf.clone()),
                        fields: vec![
                            Field::text("Note", "note", "")
                                .placeholder("Drafting the motion to compel")
                                .help("Optional now; it pre-fills the description when the timer stops."),
                        ],
                    }
                }
                FormCard {
                    title: "Log time by hand".to_string(),
                    action: "/app/projects/{code}/time-entries",
                    submit_label: "Log time".to_string(),
                    disclosure: Some(error_shown),
                    csrf_token: Some(csrf.clone()),
                    fields: vec![
                        Field::input("Date", "performed_on", "{today}", "date").required(),
                        Field::input("Minutes", "minutes", "", "number")
                            .required()
                            .help("Whole minutes, up to one day's work (1440)."),
                        Field::text("Description", "description", "")
                            .required()
                            .help("The work in one line — what the client's invoice will say."),
                        Field::checkbox("Billable", "billable", "on", false)
                            .help("Checked bills the time at your rate on this matter; unchecked records it as no-charge work."),
                    ],
                }
                if entries.is_empty() {
                    Empty {
                        title: "No time logged on this matter yet".to_string(),
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table matter-billing-entries",
                            thead {
                                tr {
                                    th { scope: "col", "Date" }
                                    th { scope: "col", "Attorney" }
                                    th { scope: "col", "Description" }
                                    th { scope: "col", "Minutes" }
                                    th { scope: "col", "Amount" }
                                    th { scope: "col", "Source" }
                                    th { scope: "col", "Status" }
                                }
                            }
                            tbody {
                                for row in entries.iter() {
                                    tr {
                                        td { "{row.performed_on}" }
                                        td { "{row.attorney}" }
                                        td { "{row.description}"
                                            if let Some(claim_id) = linked_claims.get(&row.id) {
                                                " " a { href: "#claim-{claim_id}", "Included in plan claim" }
                                            }
                                        }
                                        td { "{row.minutes}" }
                                        td {
                                            if row.billable {
                                                "{row.amount.clone().unwrap_or_default()}"
                                            } else {
                                                "—"
                                            }
                                        }
                                        td {
                                            Badge { tone: BadgeTone::Source, "{source_label(&row.source)}" }
                                        }
                                        td {
                                            if linked_claims.contains_key(&row.id) {
                                                Badge { tone: BadgeTone::Source, "Legal plan claim" }
                                            } else {
                                                Badge { tone: status_tone(&row.billing_status), "{status_label(&row.billing_status)}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if !claims.is_empty() {
                section { class: "lawyer-detail__subsection",
                    h3 { "Legal plan claims" }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table matter-billing-claims",
                            thead { tr {
                                th { scope: "col", "Plan / case number" }
                                th { scope: "col", "Benefit" }
                                th { scope: "col", "Service date" }
                                th { scope: "col", "Actual minutes" }
                                th { scope: "col", "Covered minutes" }
                                th { scope: "col", "Claim status" }
                                th { scope: "col", "Projected fee" }
                                th { scope: "col", "Filed amount" }
                                th { scope: "col", "Approved outstanding" }
                                th { scope: "col", "Net cash received" }
                            } }
                            tbody { for claim in &claims {
                                tr { key: "{claim.id}", id: "claim-{claim.id}",
                                    th { scope: "row", "{claim.plan}" br {} "{claim.case_number}" }
                                    td { "{claim.benefit}" }
                                    td { "{claim.service_date}" }
                                    td { if let Some(minutes) = claim.minutes { "{minutes}" } else { "—" } }
                                    td { {claim.coverage().unwrap_or_else(|| "—".to_owned())} }
                                    td { "{claim.status}" }
                                    td { {claim.projected.as_deref().unwrap_or("Unverified")} }
                                    td { {claim.filed.as_deref().unwrap_or("—")} }
                                    td { {claim.outstanding.as_deref().unwrap_or("—")} }
                                    td { "{claim.received}" }
                                }
                            } }
                        }
                    }
                }
            }

            section { class: "lawyer-detail__subsection matter-billing-generate",
                h3 { "Generate invoice" }
                if confirming && generate.ready {
                    {confirm_step(&code, &csrf, &generate)}
                } else if generate.resuming {
                    p { class: "nav-callout", role: "status",
                        "An earlier invoice attempt for this matter is still pending — raising \
                         again resumes that exact invoice rather than composing a second one. \
                         If it has stayed pending beyond a few minutes, ask support to \
                         reconcile it before raising."
                    }
                    form {
                        class: "lawyer-detail__inline-form",
                        method: "post",
                        action: "/app/projects/{code}/invoices",
                        input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                        button {
                            class: "nav-btn matter-billing-generate-button",
                            r#type: "submit",
                            "Resume the pending invoice"
                        }
                    }
                } else if generate.ready {
                    p { class: "nav-muted",
                        if generate.stripe_iolta.is_some() {
                            "Review the unbilled work before creating a Stripe draft."
                        } else {
                            "Compose this matter's unbilled work into one invoice and raise it in \
                             Xero. Raising is externally visible — the client's books change — so \
                             the compose is shown before anything is raised."
                        }
                    }
                    form {
                        class: "lawyer-detail__inline-form",
                        method: "post",
                        action: "/app/projects/{code}/invoices",
                        input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
                        button {
                            class: "nav-btn matter-billing-generate-button",
                            r#type: "submit",
                            "Generate invoice"
                        }
                    }
                } else {
                    Empty {
                        title: "Nothing to invoice yet".to_string(),
                        description: Some(generate.note.clone().unwrap_or_else(|| {
                            "Raising an invoice composes the matter's unbilled billable time and \
                             unbilled confirmed expenses. Log billable time or confirm an expense \
                             above, and this becomes the way to bill it."
                                .to_string()
                        })),
                    }
                }
            }

            section { class: "lawyer-detail__subsection matter-billing-invoices",
                h3 { "Invoices" }
                if invoices.is_empty() {
                    Empty {
                        title: "Nothing billed yet".to_string(),
                        description: Some(
                            "Invoices raised in Xero for this matter are mirrored here, with their \
                             payment state from the nightly reconcile.".to_string(),
                        ),
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table matter-billing-invoice-table",
                            thead {
                                tr {
                                    th { scope: "col", "Issued" }
                                    th { scope: "col", "Reference" }
                                    th { scope: "col", "Status" }
                                    th { scope: "col", "Total" }
                                    th { scope: "col", "Paid" }
                                }
                            }
                            tbody {
                                for row in invoices.iter() {
                                    tr {
                                        td { "{row.issued_on}" }
                                        td {
                                            if row.xero_invoice_id.is_empty() {
                                                "{row.reference}"
                                            } else {
                                                a {
                                                    href: "https://go.xero.com/AccountsReceivable/View.aspx?InvoiceID={row.xero_invoice_id}",
                                                    target: "_blank", rel: "noopener noreferrer",
                                                    title: "Open invoice in Xero",
                                                    "{row.reference}"
                                                }
                                            }
                                        }
                                        td {
                                            if row.settled {
                                                Badge { tone: BadgeTone::Ready, "{row.status}" }
                                            } else {
                                                Badge { tone: BadgeTone::Review, "{row.status}" }
                                            }
                                        }
                                        td { "{row.amount}" }
                                        td { "{row.paid}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            section { class: "lawyer-detail__subsection matter-billing-expenses",
                h3 { "Expenses" }
                if expenses.is_empty() {
                    Empty {
                        title: "No expenses on this matter yet".to_string(),
                        description: Some(
                            "Receipt-backed charges — filing fees, record retrieval — appear here \
                             once recorded, and ride the next invoice generated for the matter."
                                .to_string(),
                        ),
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table matter-billing-expense-table",
                            thead {
                                tr {
                                    th { scope: "col", "Date" }
                                    th { scope: "col", "Payee" }
                                    th { scope: "col", "Description" }
                                    th { scope: "col", "Amount" }
                                    th { scope: "col", "Amount status" }
                                    th { scope: "col", "Billing" }
                                }
                            }
                            tbody {
                                for row in expenses.iter() {
                                    tr {
                                        td { "{row.occurred_on}" }
                                        td { "{row.payee}" }
                                        td { "{row.description}" }
                                        td { "{row.amount}" }
                                        td {
                                            Badge {
                                                tone: expense_amount_tone(&row.amount_status),
                                                "{expense_amount_label(&row.amount_status)}"
                                            }
                                        }
                                        td {
                                            Badge {
                                                tone: expense_billing_tone(&row.billing_status),
                                                "{expense_billing_label(&row.billing_status)}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if trust.any {
                section { class: "lawyer-detail__subsection matter-billing-trust",
                    h3 { "Client trust" }
                    p { class: "nav-muted",
                        "Held for this Project in the firm's pooled client trust account, mirrored \
                         from Xero. Before transferring earned fees to operating, check the invoice, the cleared matter balance and any disputed or reserved funds."
                    }
                    ul {
                        li { "Held: {trust.held}" }
                        li { "Paid in: {trust.deposited}" }
                        li { "Earned and drawn: {trust.drawn}" }
                        li { "Refunded: {trust.refunded}" }
                    }
                    if !trust.allocations.is_empty() {
                        p { class: "nav-muted",
                            "Drawn against this Project's invoices. The pooled transfer these \
                             lines were part of is not a Project-level fact."
                        }
                        ul {
                            for line in trust.allocations.iter() {
                                li {
                                    "{line.amount} to {line.invoice_reference} on {line.occurred_on}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The confirm step: the composed lines and total, then the one button that
/// writes across the provider boundary — and the way back that doesn't. The
/// raise is externally visible (Xero is the books), so this step says what
/// is about to happen before it happens.
fn confirm_step(code: &str, csrf: &str, generate: &GenerateInvoiceView) -> Element {
    rsx! {
        p { class: "nav-muted",
            {match generate.stripe_iolta {
                Some(true) => "This creates an unsent Stripe draft. Before sending it, verify that the payment belongs in the selected trust account. Review earned fees and reimbursement of costs the Firm already paid separately.",
                Some(false) => "This creates an unsent draft in the Firm’s operating account for earned fees or reimbursable costs. Review the destination before collection.",
                None => "This records a receivable in Xero. Confirm the proper destination before requesting payment.",
            }}
        }
        if let Some(state)=&generate.trust_state { p { strong { "Trust destination: {state}" } } }
        div { class: "nav-table-wrap",
            table { class: "nav-table matter-billing-generate-preview",
                thead {
                    tr {
                        th { scope: "col", "Line" }
                        th { scope: "col", "Amount" }
                    }
                }
                tbody {
                    for line in generate.lines.iter() {
                        tr {
                            td { "{line.description}" }
                            td { "{line.amount}" }
                        }
                    }
                    tr { class: "matter-billing-generate-total",
                        td { "Total" }
                        td { "{generate.total}" }
                    }
                }
            }
        }
        form {
            class: "lawyer-detail__inline-form",
            method: "post",
            action: "/app/projects/{code}/invoices",
            input { r#type: "hidden", name: "_csrf", value: "{csrf}" }
            input { r#type: "hidden", name: "confirm", value: "on" }
            // The fingerprint of the compose the attorney just reviewed: the
            // command re-verifies it before raising, so work logged or edited
            // since this page rendered re-opens the review.
            input { r#type: "hidden", name: "digest", value: "{generate.digest}" }
            button {
                class: "nav-btn matter-billing-raise-button",
                r#type: "submit",
                if generate.stripe_iolta.is_some() { "Create Stripe draft" } else { "Raise in Xero" }
            }
        }
        a { class: "nav-link", href: "/app/projects/{code}/billing", "Not now" }
    }
}

/// The tab's rows, read once for the dashboard's server fn. The caller's
/// authorization seam has already run by the time this is called — every
/// read here is the matter's own.
#[cfg(feature = "server")]
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
pub(crate) async fn read_billing(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    project_id: uuid::Uuid,
    viewer_person_id: Option<uuid::Uuid>,
) -> Result<BillingReads, ServerFnError> {
    let entries = store::time_entries::list_for_project(pg, project_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let pending = store::billing_annotations::pending(pg, project_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let attorney_ids: Vec<uuid::Uuid> = entries
        .iter()
        .map(|row| row.attorney_id)
        .chain(pending.iter().map(|row| row.attorney_id))
        .collect();
    let names: std::collections::HashMap<uuid::Uuid, String> =
        store::persons::find_by_ids(application_store, &attorney_ids)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?
            .into_iter()
            .map(|person| (person.id, person.name))
            .collect();
    let entry_rows = entries
        .iter()
        .map(|row| {
            let amount = row.billable.then(|| {
                crate::portal_project_detail::format_usd(store::time_entries::entry_amount_cents(
                    row.minutes,
                    row.rate_cents_per_hour,
                ))
            });
            TimeEntryRow {
                id: row.id.to_string(),
                attorney: names
                    .get(&row.attorney_id)
                    .cloned()
                    .unwrap_or_else(|| "—".to_string()),
                performed_on: row.performed_on.to_string(),
                minutes: row.minutes,
                description: row.description.clone(),
                billable: row.billable,
                amount,
                source: row.source.as_str().to_string(),
                billing_status: row.billing_status.as_str().to_string(),
            }
        })
        .collect();

    let invoice_rows = mirrored_invoice_rows(application_store, project_id).await?;

    let costs = store::project_costs::list_for_project(pg, project_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let expense_rows = costs
        .iter()
        .map(|row| ExpenseRow {
            occurred_on: row.occurred_on.to_string(),
            payee: row.payee.clone(),
            description: row.description.clone(),
            amount: crate::portal_project_detail::format_usd(row.amount_cents),
            amount_status: row.amount_status.clone(),
            billing_status: row.billing_status.clone(),
        })
        .collect();

    let today = chrono::Utc::now().date_naive();
    let hourly_rate = match viewer_person_id {
        Some(person_id) => {
            store::attorney_rates::resolve_hourly_rate_cents(pg, person_id, project_id, today)
                .await
                .map_err(crate::lawyer_project_detail::server_error)?
                .map(|rate| crate::portal_project_detail::format_usd(i64::from(rate)))
        }
        None => None,
    };
    let rate_history = match viewer_person_id {
        Some(person_id) => store::attorney_rates::rates_for_person(pg, person_id)
            .await
            .map_err(crate::lawyer_project_detail::server_error)?
            .into_iter()
            .filter(|rate| rate.project_id.is_none() || rate.project_id == Some(project_id))
            .map(|rate| RateHistoryRow {
                scope: if rate.project_id.is_some() {
                    "Matter override".to_owned()
                } else {
                    "Firm default".to_owned()
                },
                effective_on: rate.effective_on.to_string(),
                hourly_rate: crate::portal_project_detail::format_usd(i64::from(
                    rate.hourly_rate_cents,
                )),
            })
            .collect(),
        None => Vec::new(),
    };
    let flat_fee = store::attorney_rates::flat_fee_cents(pg, project_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?
        .map(crate::portal_project_detail::format_usd);

    let repository_url = store::projects::find_by_id(application_store, project_id)
        .await
        .ok()
        .flatten()
        .and_then(|project| project.repository_url);
    let annotations = pending
        .into_iter()
        .map(|row| AnnotationRow {
            minutes: row.tenths * 6,
            id: row.id.to_string(),
            change_id: row.change_id,
            change_url: repository_url
                .as_deref()
                .and_then(|url| commit_url(url, &row.commit_id)),
            attorney: names
                .get(&row.attorney_id)
                .cloned()
                .unwrap_or_else(|| "Unknown timekeeper".to_owned()),
            hours: format!("{}.{}", row.tenths / 10, row.tenths % 10),
            date: row.performed_on.to_string(),
        })
        .collect();
    Ok(BillingReads {
        annotations,
        entry_rows,
        invoice_rows,
        expense_rows,
        fees: FeeStructureView {
            hourly_rate,
            rate_history,
            flat_fee,
        },
    })
}

#[cfg(feature = "server")]
fn commit_url(repository: &str, commit_id: &str) -> Option<String> {
    if !matches!(commit_id.len(), 40 | 64)
        || !commit_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let mut url = url::Url::parse(repository).ok()?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return None;
    }
    let path = url.path().trim_end_matches('/').trim_end_matches(".git");
    url.set_path(&format!("{path}/commit/{commit_id}"));
    Some(url.to_string())
}

/// The matter's invoices as the tab renders them, mapped straight from the
/// Surreal `xero_invoice` mirror — Xero is never read live. Shared by the
/// full billing read and by deployments without PostgreSQL, where the
/// workbench still shows the mirror's rows even though the time and expense
/// store is not configured.
#[cfg(feature = "server")]
pub(crate) async fn mirrored_invoice_rows(
    application_store: &store::application_store::ApplicationStore,
    project_id: uuid::Uuid,
) -> Result<Vec<InvoiceRow>, ServerFnError> {
    let mirror_invoices = store::xero_invoices::for_projects(application_store, &[project_id])
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    Ok(mirror_invoices
        .iter()
        .map(|row| InvoiceRow {
            xero_invoice_id: row.xero_invoice_id.clone(),
            reference: row.reference.clone(),
            status: row.status.clone(),
            amount: crate::portal_project_detail::format_usd(row.amount_cents),
            paid: crate::portal_project_detail::format_usd(row.amount_paid_cents),
            issued_on: row.issued_at.format("%Y-%m-%d").to_string(),
            settled: row.amount_paid_cents >= row.amount_cents && row.amount_cents > 0,
        })
        .collect())
}

/// The Billing tab's assembled rows and fee structures.
#[cfg(feature = "server")]
pub(crate) struct BillingReads {
    pub annotations: Vec<AnnotationRow>,
    pub entry_rows: Vec<TimeEntryRow>,
    pub invoice_rows: Vec<InvoiceRow>,
    pub expense_rows: Vec<ExpenseRow>,
    pub fees: FeeStructureView,
}

/// The calling attorney's open Timer, as the header widget renders it. A
/// timer on this matter offers stop and discard; one on another matter
/// renders as a link to that matter's Billing tab — the doors are
/// matter-scoped, so the capture happens there.
#[cfg(feature = "server")]
pub(crate) async fn read_timer(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    person_id: uuid::Uuid,
    project_id: uuid::Uuid,
) -> Result<Option<TimerView>, ServerFnError> {
    let running = store::timers::running_for_person(pg, person_id)
        .await
        .map_err(crate::lawyer_project_detail::server_error)?;
    let Some(timer) = running else {
        return Ok(None);
    };
    let now = chrono::Utc::now();
    let on_this_matter = timer.project_id == project_id;
    let (other_matter_code, other_matter_name) = if on_this_matter {
        (None, None)
    } else {
        match store::projects::find_by_id(application_store, timer.project_id).await {
            Ok(Some(other)) => (Some(other.code), Some(other.name)),
            // A timer whose matter has left the store still shows its clock;
            // only the link names nothing.
            _ => (None, None),
        }
    };
    Ok(Some(TimerView {
        started_at_epoch_millis: timer.started_at.timestamp_millis(),
        // The stopwatch shows real elapsed seconds — a newly started timer
        // must not open at 0:01:00. Billing minutes stay a capture-time
        // computation (the store's, never the wire's).
        elapsed: format_elapsed((now - timer.started_at).num_seconds()),
        note: timer.note,
        on_this_matter,
        other_matter_code,
        other_matter_name,
    }))
}

/// The generate-invoice action's state, composed from the same store reads
/// and the same fee rules the generation command runs
/// (`portal::project_billing_api`) — the same unbilled sets, the same
/// pending-batch resume, the same refusals. A preview that drifted from the
/// command would show one invoice and raise another, so this reads nothing
/// the command does not.
#[cfg(feature = "server")]
#[cfg_attr(feature = "server", allow(clippy::too_many_lines))]
pub(crate) async fn read_generate(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    project: &store::projects::Project,
) -> Result<GenerateInvoiceView, ServerFnError> {
    use crate::lawyer_project_detail::server_error;
    use store::firm_secrets;
    let project_id = project.id;
    let pending = store::billing_batches::pending_for_project(pg, project_id)
        .await
        .map_err(server_error)?;
    let stripe_pending = pending.as_ref().filter(|b| b.stripe_account_id.is_some());
    let use_stripe = pending.is_none() || stripe_pending.is_some();
    let route = if use_stripe {
        firm_secrets::stripe_trust_route(application_store, project)
            .await
            .map_err(server_error)?
    } else {
        None
    };
    if stripe_pending
        .is_some_and(|batch| !firm_secrets::stripe_batch_matches_route(batch, route.as_ref()))
    {
        return Ok(GenerateInvoiceView {
            note: Some(
                "Review the existing draft and its payment destination before collection.".into(),
            ),
            ..Default::default()
        });
    }
    let kind = route.as_ref().map_or(
        firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
        firm_secrets::TrustRoute::credential_kind,
    );
    let available = match project.firm_id.filter(|_| use_stripe) {
        Some(firm) => firm_secrets::stripe_credential_configured(application_store, firm, kind)
            .await
            .map_err(server_error)?,
        None => false,
    };
    if !available && (route.is_some() || stripe_pending.is_some()) {
        return Ok(GenerateInvoiceView {
            note: Some("Configure the Firm credential for the selected Stripe account before creating a draft.".into()),
            ..Default::default()
        });
    }
    let stripe_iolta = available.then_some(route.is_some());
    if pending
        .as_ref()
        .is_some_and(|batch| batch.stripe_invoice_id.is_some())
    {
        return Ok(GenerateInvoiceView {
            note: Some("A Stripe draft is awaiting review and reconciliation.".to_string()),
            ..GenerateInvoiceView::default()
        });
    }

    // A pending batch is the matter's open invoice intent: its frozen lines
    // and total are what the next raise sends, whatever was logged since —
    // new work composes the NEXT batch, never this one.
    if let Some(batch) = pending {
        let mut lines = Vec::new();
        if let serde_json::Value::Array(items) = &batch.line_items {
            for item in items {
                lines.push(PreviewLine {
                    description: item
                        .get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    amount: crate::portal_project_detail::format_usd(
                        item.get("unit_amount_cents")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0),
                    ),
                });
            }
        }
        return Ok(GenerateInvoiceView {
            ready: true,
            resuming: true,
            flat_fee: batch.flat_fee_cents.is_some(),
            total: crate::portal_project_detail::format_usd(batch.total_cents),
            lines,
            note: None,
            // A pending batch is frozen: its lines and total are what the
            // next raise sends, so there is nothing fresh to fingerprint.
            digest: String::new(),
            stripe_iolta,
            trust_state: batch.trust_state,
        });
    }

    let costs_only =
        store::billing_batches::costs_only(pg, project_id, chrono::Utc::now().date_naive())
            .await
            .map_err(server_error)?;
    let entries = store::time_entries::unbilled_billable(pg, project_id)
        .await
        .map_err(server_error)?;
    let unbilled_costs = store::project_costs::list_for_project(pg, project_id)
        .await
        .map_err(server_error)?
        .into_iter()
        .filter(|cost| cost.amount_status == "confirmed" && cost.billing_status == "unbilled")
        .collect::<Vec<_>>();
    let agreed_fee_cents = store::attorney_rates::flat_fee_cents(pg, project_id)
        .await
        .map_err(server_error)?;
    let agreed_fee_cents = agreed_fee_cents.filter(|_| !costs_only);
    let entries = if costs_only { Vec::new() } else { entries };
    let billed_fee_cents = store::billing_batches::flat_fee_billed_cents(pg, project_id)
        .await
        .map_err(server_error)?;
    let flat_fee_cents =
        agreed_fee_cents.and_then(|fee| (fee > billed_fee_cents).then_some(fee - billed_fee_cents));
    let fee_billed = agreed_fee_cents.is_some() && flat_fee_cents.is_none();

    // Mirror the command's rule refusals as a not-ready action with the
    // sentence that says why — the button would only refuse.
    if fee_billed && unbilled_costs.is_empty() {
        return Ok(GenerateInvoiceView {
            note: Some(
                "The matter's agreed flat fee has already been billed; only new expenses bill \
                 independently of it."
                    .to_string(),
            ),
            ..GenerateInvoiceView::default()
        });
    }
    let today = chrono::Utc::now().date_naive();
    if let Some(cost) = unbilled_costs.iter().find(|cost| cost.occurred_on > today) {
        return Ok(GenerateInvoiceView {
            note: Some(format!(
                "The {} expense from {} is dated after today — resolve its date before invoicing.",
                cost.description, cost.occurred_on
            )),
            ..GenerateInvoiceView::default()
        });
    }

    // Under a consumed fee agreement the matter's time is covered by the
    // fee — the command empties the entries exactly here, so a cost-only
    // batch is what the preview must show. Billing covered time at hourly
    // rates would charge the client twice for the same agreement.
    let entries = if fee_billed { Vec::new() } else { entries };

    // The compose, in the command's own order and wording: the fee as one
    // line covering the time when agreed; otherwise one line per time entry
    // at its snapshot rate; expenses always itemized — and never in a
    // currency the invoice cannot bill, which the command refuses whole.
    for cost in &unbilled_costs {
        if cost.currency != store::billing_batches::INVOICE_CURRENCY {
            return Ok(GenerateInvoiceView {
                note: Some(format!(
                    "An unbilled expense is in {}; matter invoices are raised in USD.",
                    cost.currency
                )),
                ..GenerateInvoiceView::default()
            });
        }
    }
    let digest = store::billing_batches::invoice_destination_digest(
        store::billing_batches::preview_digest(&entries, &unbilled_costs, flat_fee_cents),
        stripe_iolta,
        project.firm_id,
    );
    let digest = match &route {
        Some(route) => route.bind_preview(&digest),
        None => digest,
    };
    let mut lines = Vec::new();
    let mut total_cents: i64 = 0;
    if let Some(fee_cents) = flat_fee_cents {
        lines.push(PreviewLine {
            description: "Flat fee — legal services per the fee agreement".to_string(),
            amount: crate::portal_project_detail::format_usd(fee_cents),
        });
        total_cents += fee_cents;
    } else {
        for entry in &entries {
            let cents =
                store::time_entries::entry_amount_cents(entry.minutes, entry.rate_cents_per_hour);
            lines.push(PreviewLine {
                description: format!("{} — {}", entry.description, entry.performed_on),
                amount: crate::portal_project_detail::format_usd(cents),
            });
            total_cents += cents;
        }
    }
    for cost in &unbilled_costs {
        lines.push(PreviewLine {
            description: format!("{} — {}", cost.description, cost.payee),
            amount: crate::portal_project_detail::format_usd(cost.amount_cents),
        });
        total_cents += cost.amount_cents;
    }

    Ok(GenerateInvoiceView {
        ready: !lines.is_empty() && total_cents != 0,
        lines,
        total: crate::portal_project_detail::format_usd(total_cents),
        resuming: false,
        flat_fee: flat_fee_cents.is_some(),
        note: None,
        digest,
        stripe_iolta,
        trust_state: route.map(|route| route.state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_renders_as_a_stopwatch() {
        assert_eq!(format_elapsed(0), "0:00:00");
        assert_eq!(format_elapsed(59), "0:00:59");
        assert_eq!(format_elapsed(60), "0:01:00");
        assert_eq!(format_elapsed(3661), "1:01:01");
        // Clock skew is clamped, never negative.
        assert_eq!(format_elapsed(-5), "0:00:00");
    }

    #[test]
    fn billing_status_words_map_to_labels_and_tones() {
        assert_eq!(status_label("unbilled"), "Unbilled");
        assert_eq!(status_label("reserved"), "Reserved");
        assert_eq!(status_label("invoiced"), "Invoiced");
        assert_eq!(status_label("written_off"), "Written off");
        assert_eq!(status_tone("unbilled"), BadgeTone::Next);
        assert_eq!(status_tone("reserved"), BadgeTone::Review);
        assert_eq!(status_tone("invoiced"), BadgeTone::Ready);
        assert_eq!(status_tone("written_off"), BadgeTone::Blocked);
    }

    #[test]
    fn expense_words_map_to_labels_and_tones() {
        assert_eq!(expense_amount_label("estimated"), "Estimated");
        assert_eq!(expense_amount_label("confirmed"), "Confirmed");
        assert_eq!(expense_billing_label("unbilled"), "Unbilled");
        assert_eq!(expense_billing_label("resolved"), "Resolved");
        assert_eq!(expense_amount_tone("confirmed"), BadgeTone::Source);
        assert_eq!(expense_billing_tone("invoiced"), BadgeTone::Ready);
    }

    #[test]
    fn the_timer_widget_ticks_from_the_epoch_attribute_with_the_label_as_fallback() {
        let timer = TimerView {
            started_at_epoch_millis: 1_730_000_000_000,
            elapsed: "0:00:12".to_string(),
            note: Some("Drafting the motion".to_string()),
            on_this_matter: true,
            other_matter_code: None,
            other_matter_name: None,
        };
        let html = dioxus_ssr::render_element(rsx! {
            MatterTimer {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                timer: timer.clone(),
            }
        });
        assert!(
            html.contains("data-timer-started-at=\"1730000000000\""),
            "missing [data-timer-started-at=\"1730000000000\"]"
        );
        assert!(html.contains("0:00:12"), "missing [0:00:12]");
        // The stop door captures the entry; the discard door records nothing.
        assert!(
            html.contains("/app/projects/acme/timer/stop"),
            "missing [/app/projects/acme/timer/stop]"
        );
        assert!(
            html.contains("/app/projects/acme/timer/discard"),
            "missing [/app/projects/acme/timer/discard]"
        );
        // The widget script is a document-head element — bare `dioxus_ssr`
        // renders it through the no-op document provider, so its presence is
        // the browser e2e's assertion, not this one.
        // The note pre-fills the capture description — editable, since the
        // stop's submit is what creates the entry.
        assert!(
            html.contains("Drafting the motion"),
            "missing [Drafting the motion]"
        );
        assert!(
            html.contains("name=\"description\""),
            "missing [name=\"description\"]"
        );
        // The billable choice rides the same submit, checked by default.
        assert!(
            html.contains("name=\"billable\""),
            "missing [name=\"billable\"]"
        );
        assert!(html.contains("checked"), "missing [checked]");
    }

    #[test]
    fn a_timer_on_another_matter_links_there_instead_of_offering_the_doors() {
        let timer = TimerView {
            started_at_epoch_millis: 1_730_000_000_000,
            elapsed: "0:04:00".to_string(),
            note: None,
            on_this_matter: false,
            other_matter_code: Some("other".to_string()),
            other_matter_name: Some("Other Matter".to_string()),
        };
        let html = dioxus_ssr::render_element(rsx! {
            MatterTimer {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                timer,
            }
        });
        assert!(
            html.contains("/app/projects/other/billing"),
            "missing [/app/projects/other/billing]"
        );
        assert!(
            !html.contains("/app/projects/acme/timer/stop"),
            "unexpectedly present: [/app/projects/acme/timer/stop]"
        );
        assert!(
            !html.contains("/app/projects/acme/timer/discard"),
            "unexpectedly present: [/app/projects/acme/timer/discard]"
        );
    }

    fn ssr_billing(entries: Vec<TimeEntryRow>, invoices: Vec<InvoiceRow>) -> String {
        ssr_billing_fees(
            entries,
            invoices,
            Some("$250.00"),
            GenerateInvoiceView::default(),
            false,
        )
    }

    fn ssr_billing_with(
        entries: Vec<TimeEntryRow>,
        invoices: Vec<InvoiceRow>,
        generate: GenerateInvoiceView,
        confirming: bool,
    ) -> String {
        ssr_billing_fees(entries, invoices, Some("$250.00"), generate, confirming)
    }

    /// `hourly_rate: None` renders the no-rate fee copy — the empty-sections
    /// contract needs a matter with no fee structure on file at all.
    fn ssr_billing_fees(
        entries: Vec<TimeEntryRow>,
        invoices: Vec<InvoiceRow>,
        hourly_rate: Option<&str>,
        generate: GenerateInvoiceView,
        confirming: bool,
    ) -> String {
        dioxus_ssr::render_element(rsx! {
            BillingTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                confirming,
                timer: None,
                entries,
                fees: FeeStructureView {
                    hourly_rate: hourly_rate.map(String::from),
                    rate_history: Vec::new(),
                    flat_fee: None,
                },
                invoices,
                expenses: Vec::new(),
                trust: TrustView::default(),
                generate,
                today: "2026-11-04".to_string(),
            }
        })
    }

    #[test]
    fn plan_time_review_offers_the_matching_claims_for_explicit_selection() {
        let id = uuid::Uuid::from_u128(11);
        let other = uuid::Uuid::from_u128(12);
        let stale = uuid::Uuid::from_u128(13);
        let claims = vec![
            (id, "2026-11-03"),
            (other, "2026-11-03"),
            (stale, "2026-11-02"),
        ]
        .into_iter()
        .map(|(id, date)| PlanClaimRow {
            id,
            plan: "Synthetic Plan".into(),
            case_number: "SYNTHETIC".into(),
            benefit: format!("Synthetic service {id}"),
            service_date: date.into(),
            minutes: Some(18),
            covered_minutes: Some(240),
            annotation_price: Some("Verified terms".into()),
            status: "Completed".into(),
            projected: None,
            filed: None,
            outstanding: None,
            received: String::new(),
            time_entry_ids: Vec::new(),
        })
        .collect::<Vec<_>>();
        let annotations = vec![AnnotationRow {
            id: "synthetic".into(),
            change_id: "synthetic-change".into(),
            change_url: None,
            attorney: "Synthetic Lawyer".into(),
            hours: "0.3".into(),
            date: "2026-11-03".into(),
            minutes: 18,
        }];
        let html = dioxus_ssr::render_element(rsx! {
            BillingTab {
                code: "acme".to_owned(), csrf: "synthetic".to_owned(), timer: None,
                entries: Vec::new(), claims, annotations, plan_linked: true,
                fees: FeeStructureView::default(), invoices: Vec::new(), expenses: Vec::new(),
                trust: TrustView::default(), generate: GenerateInvoiceView::default(),
                today: "2026-11-04".to_owned(),
            }
        });
        assert!(html.contains("Approve to claim"));
        assert!(html.contains(&format!("value=\"{id}\"")));
        assert!(html.contains(&format!("value=\"{other}\"")));
        assert!(!html.contains(&format!("value=\"{stale}\"")));
        assert!(html.contains("18 of 240 used"));
        assert!(!html.contains("No hourly rate is on file"));
    }

    #[test]
    fn the_tab_renders_entries_with_statuses_and_unpriced_non_billable_work() {
        let entries = vec![
            TimeEntryRow {
                id: "e1".to_string(),
                attorney: "A. Rivera".to_string(),
                performed_on: "2026-11-03".to_string(),
                minutes: 90,
                description: "Drafted the motion to compel".to_string(),
                billable: true,
                amount: Some("$375.00".to_string()),
                source: "timer".to_string(),
                billing_status: "unbilled".to_string(),
            },
            TimeEntryRow {
                id: "e2".to_string(),
                attorney: "N. Shook".to_string(),
                performed_on: "2026-11-02".to_string(),
                minutes: 30,
                description: "Client call".to_string(),
                billable: false,
                amount: None,
                source: "manual".to_string(),
                billing_status: "written_off".to_string(),
            },
        ];
        let html = ssr_billing(entries, Vec::new());
        assert!(
            html.contains("Drafted the motion to compel"),
            "missing [Drafted the motion to compel]"
        );
        assert!(html.contains("$375.00"), "missing [$375.00]");
        assert!(html.contains("Unbilled"), "missing [Unbilled]");
        // Non-billable work renders unpriced, not $0.
        assert!(html.contains("Written off"), "missing [Written off]");
        assert!(html.contains("—"), "missing [—]");
        assert!(html.contains("Timer"), "missing [Timer]");
        assert!(html.contains("Manual"), "missing [Manual]");
        // The log-time form posts to the matter's native door.
        assert!(
            html.contains("/app/projects/acme/time-entries"),
            "missing [/app/projects/acme/time-entries]"
        );
    }

    #[test]
    fn each_invoice_links_its_reference_to_its_xero_record() {
        let invoices = vec![InvoiceRow {
            reference: "INV-123".into(),
            xero_invoice_id: "test-invoice".into(),
            amount: "$400.00".into(),
            status: "AUTHORISED".into(),
            ..Default::default()
        }];
        let html = ssr_billing(Vec::new(), invoices);
        assert!(html.contains("$400.00"), "missing [$400.00]");
        assert!(html.contains("AUTHORISED"), "missing [AUTHORISED]");
        assert!(html
            .contains("https://go.xero.com/AccountsReceivable/View.aspx?InvoiceID=test-invoice"));
        assert_eq!(html.matches("INV-123").count(), 1);
    }

    #[test]
    fn empty_sections_explain_themselves_rather_than_blank() {
        let html = ssr_billing_fees(
            Vec::new(),
            Vec::new(),
            None,
            GenerateInvoiceView::default(),
            false,
        );
        assert!(
            html.contains("No time logged on this matter yet"),
            "missing [No time logged on this matter yet]"
        );
        assert!(
            html.contains("Nothing billed yet"),
            "missing [Nothing billed yet]"
        );
        assert!(
            html.contains("No expenses on this matter yet"),
            "missing [No expenses on this matter yet]"
        );
        // The fee copy covers both absences.
        assert!(
            html.contains("No hourly rate is on file for you on this matter"),
            "missing [No hourly rate is on file for you on this matter]"
        );
        assert!(
            html.contains("No flat fee is recorded"),
            "missing [No flat fee is recorded]"
        );
        // A matter with no trust postings says nothing about trust.
        assert!(
            !html.contains("Client trust"),
            "unexpectedly present: [Client trust]"
        );
    }

    #[test]
    fn the_fee_structure_renders_resolved_rates() {
        let html = dioxus_ssr::render_element(rsx! {
            BillingTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                timer: None,
                entries: Vec::new(),
                fees: FeeStructureView {
                    hourly_rate: Some("$325.00".to_string()),
                    rate_history: Vec::new(),
                    flat_fee: Some("$5,000.00".to_string()),
                },
                invoices: Vec::new(),
                expenses: Vec::new(),
                trust: TrustView::default(),
                generate: GenerateInvoiceView::default(),
                today: "2026-11-04".to_string(),
            }
        });
        assert!(html.contains("$325.00"), "missing [$325.00]");
        assert!(html.contains("$5,000.00"), "missing [$5,000.00]");
        assert!(
            html.contains("Flat fee agreed"),
            "missing [Flat fee agreed]"
        );
    }

    #[test]
    fn missing_rate_review_error_links_to_fee_structure() {
        let html = dioxus_ssr::render_element(rsx! {
            BillingTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                error: Some("Set the hourly rate for the date of service before approving billable time.".to_string()),
                timer: None,
                entries: Vec::new(),
                fees: FeeStructureView::default(),
                invoices: Vec::new(),
                expenses: Vec::new(),
                trust: TrustView::default(),
                generate: GenerateInvoiceView::default(),
                today: "2026-11-25".to_string(),
            }
        });
        assert!(html.contains("href=\"#fee-structure\""));
        assert!(html.contains("id=\"fee-structure\""));
    }

    #[test]
    fn fee_structure_explains_precedence_and_lists_visible_rate_changes() {
        let html = dioxus_ssr::render_element(rsx! {
            BillingTab {
                code: "acme".to_string(),
                csrf: "token".to_string(),
                timer: None,
                entries: Vec::new(),
                fees: FeeStructureView {
                    hourly_rate: Some("$100.00".to_string()),
                    rate_history: vec![RateHistoryRow {
                        scope: "Matter override".to_string(),
                        effective_on: "2026-11-01".to_string(),
                        hourly_rate: "$100.00".to_string(),
                    }],
                    flat_fee: None,
                },
                invoices: Vec::new(),
                expenses: Vec::new(),
                trust: TrustView::default(),
                generate: GenerateInvoiceView::default(),
                today: "2026-11-25".to_string(),
            }
        });
        assert!(html.contains("matter override takes effect"));
        assert!(html.contains("firm-wide rate applies"));
        assert!(html.contains("Rate history"));
        assert!(html.contains("2026-11-01"));
        assert!(html.contains("/app/projects/acme/rates"));
    }

    #[test]
    #[cfg(feature = "server")]
    fn staged_change_links_to_its_repository_commit() {
        assert_eq!(
            commit_url(
                "https://forge.example/firm/matter.git",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .as_deref(),
            Some(
                "https://forge.example/firm/matter/commit/0123456789abcdef0123456789abcdef01234567"
            )
        );
        assert!(commit_url(
            "javascript:alert(1)",
            "0123456789abcdef0123456789abcdef01234567"
        )
        .is_none());
    }

    #[test]
    fn a_ready_matter_offers_the_generate_action_whose_first_step_is_the_compose() {
        let html = ssr_billing_with(
            Vec::new(),
            Vec::new(),
            GenerateInvoiceView {
                ready: true,
                ..GenerateInvoiceView::default()
            },
            false,
        );
        assert!(
            html.contains("/app/projects/acme/invoices"),
            "missing [/app/projects/acme/invoices]"
        );
        assert!(
            html.contains("Generate invoice"),
            "missing [Generate invoice]"
        );
        // The compose-before-raise promise is on the action's face.
        assert!(
            html.contains("the compose is shown before anything is raised"),
            "missing [the compose is shown before anything is raised]"
        );
    }

    #[test]
    fn nothing_to_invoice_is_a_disabled_action_with_an_explanation() {
        let html = ssr_billing_with(
            Vec::new(),
            Vec::new(),
            GenerateInvoiceView::default(),
            false,
        );
        assert!(
            html.contains("Nothing to invoice yet"),
            "missing [Nothing to invoice yet]"
        );
        // The section heading names the action; the action itself — the
        // button — must not render when there is nothing to invoice.
        assert!(
            !html.contains("matter-billing-generate-button"),
            "unexpectedly present: [matter-billing-generate-button]"
        );
    }

    #[test]
    fn a_pending_batch_resumes_instead_of_composing_anew() {
        let html = ssr_billing_with(
            Vec::new(),
            Vec::new(),
            GenerateInvoiceView {
                ready: true,
                resuming: true,
                ..GenerateInvoiceView::default()
            },
            false,
        );
        assert!(
            html.contains("Resume the pending invoice"),
            "missing [Resume the pending invoice]"
        );
        assert!(
            html.contains("resumes that exact invoice"),
            "missing [resumes that exact invoice]"
        );
    }

    #[test]
    fn the_confirm_step_shows_the_compose_and_the_one_button_that_raises() {
        let html = ssr_billing_with(
            Vec::new(),
            Vec::new(),
            GenerateInvoiceView {
                ready: true,
                note: None,
                lines: vec![
                    PreviewLine {
                        description: "Drafted the motion to compel — 2026-11-03".to_string(),
                        amount: "$375.00".to_string(),
                    },
                    PreviewLine {
                        description: "Filing fee — Hamilton County".to_string(),
                        amount: "$120.00".to_string(),
                    },
                ],
                total: "$495.00".to_string(),
                resuming: false,
                flat_fee: false,
                digest: "digest-of-the-reviewed-compose".to_string(),
                stripe_iolta: None,
                trust_state: None,
            },
            true,
        );
        assert!(
            html.contains("Drafted the motion to compel — 2026-11-03"),
            "missing [Drafted the motion to compel — 2026-11-03]"
        );
        assert!(html.contains("$120.00"), "missing [$120.00]");
        assert!(html.contains("$495.00"), "missing [$495.00]");
        // The one provider-boundary write, plus the way back that isn't one.
        assert!(
            html.contains("matter-billing-raise-button"),
            "missing [matter-billing-raise-button]"
        );
        assert!(html.contains("Raise in Xero"), "missing [Raise in Xero]");
        assert!(
            html.contains("name=\"confirm\" value=\"on\""),
            "missing [name=\"confirm\" value=\"on\"]"
        );
        // The confirmation is bound to the compose the attorney reviewed:
        // its fingerprint rides the form, and the command re-verifies it
        // against a fresh compose before anything is raised.
        assert!(
            html.contains("name=\"digest\" value=\"digest-of-the-reviewed-compose\""),
            "missing [name=\"digest\" value=\"digest-of-the-reviewed-compose\"]"
        );
        assert!(html.contains(r#"href="/app/projects/acme/billing""#));
        assert!(html.contains("records a receivable in Xero"));
    }

    // ---- the server-side reads, against PostgreSQL ----
    #[cfg(feature = "server")]
    mod server_reads {}
}
