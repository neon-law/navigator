//! Matter-level invoice generation — the first production caller of the
//! [`billing::BillingProvider`] seam beyond the self-serve purchases flow.
//!
//! One command composes a matter's unbilled work into one invoice and raises
//! it through the provider seam: unbilled billable Time Entries at their
//! snapshotted rates plus unbilled confirmed Project Costs (the same parts
//! `store::matter_financials` prices Work In Progress from), or — when the
//! matter carries an agreed flat fee — the fee itself plus those costs. The
//! raised invoice is mirrored locally (`store::xero_invoices::upsert`, the
//! same ingest path the nightly reconcile uses), and the consumed entries and
//! costs flip to invoiced under the Xero id through their own audited
//! transitions (`time_entries::mark_invoiced`, `project_costs::link_invoice`).
//!
//! **Xero is the books.** The only write across the provider boundary is
//! `create_invoice` — idempotent on the caller-supplied key at the seam (Xero
//! honours the `Idempotency-Key` for six minutes). This command keys it on a **billing
//! batch**: the composed request and the row ids it consumes are persisted —
//! and their rows reserved — BEFORE the provider call, so a retry resumes the
//! same batch under the same key within the safe retry window. New work
//! after an earlier invoice composes a new batch and raises a new invoice. Reservation closes the
//! write-off race: a reserved entry cannot be written off and a reserved cost
//! cannot be resolved or re-confirmed, so the work the provider bills is the
//! work the lawyer saw on the confirmation step. On a provider refusal
//! nothing is invoiced and nothing is mirrored; the work stays reserved under
//! the pending batch and the retry completes it. Every other write here is
//! local state describing what Xero now holds.
//!
//! A matter whose flat fee is agreed bills the fee rather than itemized time:
//! the fee is the agreed price of the matter's work, so its unbilled entries
//! are consumed by (marked under) the fee invoice and never billed again. The
//! fee is frozen into the batch, so a retry bills the agreed fee even if the
//! agreement changed meanwhile.
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule), exactly as [`crate::project_costs_api`];
//! the matter-scope question is asked once through
//! [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row — a client, a clerk, an unassigned lawyer, Owner and
//! Admin included — gets the non-disclosing 404, never a refusal.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use store::{
    attorney_rates, billing_batches, postgres_mirror, project_costs as costs, projects,
    time_entries, xero_invoices,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::billing::{self, InvoiceLine, InvoiceRequest};

/// The currency a generated invoice bills in — the deployment's receivables
/// default, the same one the mirror's ingest falls back to. A confirmed
/// unbilled cost in another currency refuses the whole generation (before
/// anything is raised) rather than silently dropping reimbursable money or
/// mixing currencies on one Xero invoice.
const INVOICE_CURRENCY: &str = "USD";

/// The Xero chart-of-accounts code the revenue posts to — the same sales code
/// the services catalog copy and the sandbox-reset fixture bill against.
const REVENUE_ACCOUNT_CODE: &str = "200";

// Xero retains keys for six minutes from the first call:
// https://developer.xero.com/documentation/guides/idempotent-requests/idempotency/
// Persist the first attempt before POST. A four-minute start cutoff plus a
// 60-second attempt budget leaves at least one minute of transport margin.
const CREATE_ATTEMPT_BUDGET: std::time::Duration = std::time::Duration::from_secs(60);

fn create_window_open(first_attempted_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    let age = now.signed_duration_since(first_attempted_at);
    (chrono::Duration::zero()..chrono::Duration::minutes(4)).contains(&age)
}

/// How a Xero `Reference` names the matter an invoice bills — the documented
/// ingest convention (`store::xero_invoices::ProjectScopeRef`).
fn matter_reference(code: &str) -> String {
    format!("Matter {code}")
}

/// The composition of a matter's unbilled work into invoice lines: the pure
/// middle of the command, no store or provider on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Compose {
    line_items: Vec<InvoiceLine>,
    total_cents: i64,
}

/// Compose the invoice lines for a matter: one line per unbilled billable
/// Time Entry at its snapshotted rate (the same per-entry amount Work In
/// Progress shows), one per unbilled confirmed Project Cost, or — when the
/// matter carries an agreed flat fee — the fee as one line covering the
/// entries. Money is integer cents throughout; quantity is always 1 because
/// each line is already priced in cents, never in fractional hours.
fn compose_lines(
    entries: &[time_entries::TimeEntry],
    unbilled_costs: &[costs::ProjectCost],
    flat_fee_cents: Option<i64>,
) -> Result<Compose, InvoiceGenerationError> {
    for cost in unbilled_costs {
        if cost.currency != INVOICE_CURRENCY {
            return Err(InvoiceGenerationError::CurrencyMismatch(
                cost.currency.clone(),
            ));
        }
    }

    let mut line_items = Vec::with_capacity(entries.len() + unbilled_costs.len() + 1);
    if let Some(fee_cents) = flat_fee_cents {
        // The fee is the agreed price of the matter's work: one line, and the
        // entries below are consumed under it rather than itemized.
        line_items.push(InvoiceLine {
            description: "Flat fee — legal services per the fee agreement".to_string(),
            quantity: 1,
            unit_amount_cents: fee_cents,
            account_code: REVENUE_ACCOUNT_CODE.to_string(),
        });
    } else {
        for entry in entries {
            line_items.push(InvoiceLine {
                description: format!("{} — {}", entry.description, entry.performed_on),
                quantity: 1,
                unit_amount_cents: time_entries::entry_amount_cents(
                    entry.minutes,
                    entry.rate_cents_per_hour,
                ),
                account_code: REVENUE_ACCOUNT_CODE.to_string(),
            });
        }
    }
    for cost in unbilled_costs {
        line_items.push(InvoiceLine {
            description: format!("{} — {}", cost.description, cost.payee),
            quantity: 1,
            unit_amount_cents: cost.amount_cents,
            account_code: REVENUE_ACCOUNT_CODE.to_string(),
        });
    }

    let total_cents = line_items
        .iter()
        .map(|line| i64::from(line.quantity) * line.unit_amount_cents)
        .sum();
    Ok(Compose {
        line_items,
        total_cents,
    })
}

/// What the matter owes a billed party: the client DRI when the matter names
/// one, else the first client-side participant. `None` when the matter has no
/// client-side participation at all — there is nobody to address the
/// invoice to, and the command refuses rather than guess.
async fn billed_party(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
) -> Result<Option<store::persons::Person>, InvoiceGenerationError> {
    let participations = projects::participations_for_project(surreal, project_id).await?;
    let client_side = participations
        .into_iter()
        .filter(|row| {
            projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
                || row.is_client_dri
        })
        .collect::<Vec<_>>();
    let party = client_side
        .iter()
        .find(|row| row.is_client_dri)
        .or_else(|| client_side.first());
    match party {
        Some(row) => Ok(store::persons::find_by_id(surreal, row.person_id).await?),
        None => Ok(None),
    }
}

/// One raised invoice as the command answers it: what was billed, the parts
/// that were consumed under it, and the Xero id every local flip carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InvoiceReceipt {
    pub xero_invoice_id: String,
    pub reference: String,
    pub currency: String,
    pub total_cents: i64,
    pub line_items: Vec<InvoiceLine>,
    /// Time Entries consumed under the invoice. On a flat-fee matter these
    /// are covered by the fee line, not itemized on it.
    pub consumed_time_entries: usize,
    pub consumed_costs: usize,
    /// Whether the invoice billed the matter's agreed flat fee instead of
    /// itemized time.
    pub flat_fee: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvoiceGenerationError {
    #[error("nothing is unbilled on this matter")]
    NothingToInvoice,
    #[error("an unbilled cost is in {0}; a matter invoice bills in USD")]
    CurrencyMismatch(String),
    #[error("the matter names no client to bill")]
    NoBilledParty,
    #[error("an unbilled cost is dated {0}, after today's invoice")]
    FutureDatedCost(NaiveDate),
    #[error("the matter's flat fee is already billed — amend the fee for additional work")]
    FlatFeeAlreadyBilled,
    #[error("the pending invoice requires reconciliation before another provider attempt")]
    ReconciliationRequired,
    #[error("the provider invoice is not authorised or paid; reconciliation is required")]
    InvoiceNotBillable,
    #[error(transparent)]
    Batch(#[from] billing_batches::BillingBatchError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("the matter is not on the store")]
    NoMatter,
    #[error(transparent)]
    Time(#[from] time_entries::TimeEntryError),
    #[error(transparent)]
    Fee(#[from] attorney_rates::FeeStructureError),
    #[error(transparent)]
    Cost(#[from] costs::ProjectCostError),
    #[error(transparent)]
    Billing(#[from] billing::BillingError),
    #[error(transparent)]
    Mirror(#[from] Box<xero_invoices::XeroInvoiceError>),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error(transparent)]
    Person(#[from] store::persons::PersonError),
}

/// Generate the matter's invoice: compose the unbilled work, persist the
/// billing batch with its rows reserved, raise one invoice through the
/// provider seam keyed on the batch id, mirror it locally, and flip the
/// consumed entries and costs to invoiced under the Xero id. `now` is the
/// reservation timestamp and cost-date cutoff; the caller's authorization
/// seam governs who may call.
///
/// # Errors
///
/// [`InvoiceGenerationError`] when the matter has nothing unbilled, names no
/// client, carries a cost in another currency or dated after the invoice, or
/// the store or provider refuses. Refusals before the batch is persisted
/// change nothing; afterwards the work stays reserved under the pending
/// batch and the retry resumes it — the provider call is keyed on the batch
/// id within a bounded retry window; older uncertain attempts require
/// reconciliation.
pub(crate) async fn generate_invoice(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    project_id: Uuid,
    actor_id: Uuid,
    now: DateTime<Utc>,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    let project = projects::find_by_id(surreal, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;

    // A pending batch is the matter's open invoice intent: its request and
    // rows were frozen (and reserved) when this command last ran, so the
    // retry resumes that exact batch instead of recomposing a different one —
    // new work logged meanwhile composes the NEXT batch, it never slips into
    // this one.
    if let Some(batch) = billing_batches::pending_for_project(pg, project_id).await? {
        return raise_batch(surreal, pg, provider, project_id, &batch, actor_id).await;
    }

    let entries = time_entries::unbilled_billable(pg, project_id).await?;
    let unbilled_costs = costs::list_for_project(pg, project_id)
        .await?
        .into_iter()
        .filter(|cost| cost.amount_status == "confirmed" && cost.billing_status == "unbilled")
        .collect::<Vec<_>>();
    let agreed_fee_cents = attorney_rates::flat_fee_cents(pg, project_id).await?;
    let billed_fee_cents = billing_batches::flat_fee_billed_cents(pg, project_id).await?;
    let flat_fee_cents =
        agreed_fee_cents.and_then(|fee| (fee > billed_fee_cents).then_some(fee - billed_fee_cents));
    let fee_billed = agreed_fee_cents.is_some() && flat_fee_cents.is_none();

    // The zero-unbilled no-op: with no fee agreed and nothing unbilled there
    // is nothing to compose, and the provider is never called — a repeat of a
    // successful generation lands here, not on Xero. A fee agreement always
    // bills, even on a matter that tracks no time.
    if flat_fee_cents.is_none() && entries.is_empty() && unbilled_costs.is_empty() {
        return Err(InvoiceGenerationError::NothingToInvoice);
    }
    // Under a consumed fee agreement the matter's work is covered by the
    // fee: billing later time at hourly rates would charge the client twice
    // for the same agreement. The firm amends the fee for additional work;
    // generation refuses until then.
    if fee_billed {
        return Err(InvoiceGenerationError::FlatFeeAlreadyBilled);
    }
    // A cost dated after the invoice composes fine but would refuse at
    // link_invoice — after Xero already holds the invoice. The same rule
    // runs here, before anything is raised.
    let today = now.date_naive();
    if let Some(cost) = unbilled_costs.iter().find(|cost| cost.occurred_on > today) {
        return Err(InvoiceGenerationError::FutureDatedCost(cost.occurred_on));
    }
    let compose = compose_lines(&entries, &unbilled_costs, flat_fee_cents)?;

    let Some(party) = billed_party(surreal, project_id).await? else {
        return Err(InvoiceGenerationError::NoBilledParty);
    };
    // Resolve the billed party first — the seam's documented order — then
    // reserve the batch's rows and persist the intent BEFORE the provider
    // call. Reservation is what makes the invoice's work final: a reserved
    // entry cannot be written off and a reserved cost cannot be resolved or
    // re-confirmed, so nothing the lawyer removed can slip into the batch
    // while Xero is deciding.
    provider
        .ensure_contact(&billing::ContactRequest {
            name: party.name.clone(),
            email: party.email.clone(),
        })
        .await?;
    let entry_ids = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
    let cost_ids = unbilled_costs
        .iter()
        .map(|cost| cost.id)
        .collect::<Vec<_>>();
    // Reservation and intent persist in ONE transaction: a crash between
    // them cannot strand reserved work with no batch to resume it.
    let batch = billing_batches::insert_with_reservations(
        pg,
        &billing_batches::MatterBillingBatch {
            id: Uuid::new_v4(),
            project_id,
            status: "pending".to_string(),
            reference: matter_reference(&project.code),
            contact_name: party.name.clone(),
            contact_email: party.email.clone(),
            first_attempted_at: None,
            line_items: serde_json::to_value(&compose.line_items)?,
            total_cents: compose.total_cents,
            currency: INVOICE_CURRENCY.to_string(),
            entry_ids,
            cost_ids,
            xero_invoice_id: None,
            flat_fee_cents,
            actor_id,
            inserted_at: now,
            updated_at: now,
        },
        agreed_fee_cents.map(|_| billed_fee_cents),
    )
    .await?;
    raise_batch(surreal, pg, provider, project_id, &batch, actor_id).await
}

async fn invoice_for_batch(
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    batch: &billing_batches::MatterBillingBatch,
    line_items: &[InvoiceLine],
) -> Result<(billing::InvoiceId, DateTime<Utc>), InvoiceGenerationError> {
    if let Some(saved) = &batch.xero_invoice_id {
        return Ok((billing::InvoiceId(saved.clone()), batch.updated_at));
    }
    if batch
        .first_attempted_at
        .is_some_and(|first| !create_window_open(first, Utc::now()))
    {
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    provider
        .ensure_contact(&billing::ContactRequest {
            name: batch.contact_name.clone(),
            email: batch.contact_email.clone(),
        })
        .await?;
    let attempted = billing_batches::start_attempt(pg, batch.id, Utc::now()).await?;
    if let Some(saved) = attempted.xero_invoice_id {
        return Ok((billing::InvoiceId(saved), attempted.updated_at));
    }
    let first = attempted
        .first_attempted_at
        .ok_or(billing_batches::BillingBatchError::Invalid(
            "first attempt timestamp",
        ))?;
    if !create_window_open(first, Utc::now()) {
        // An uncertain result outside Xero's key-retention budget must be
        // reconciled manually. Never guess by matter reference or total.
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    let invoice = tokio::time::timeout(
        CREATE_ATTEMPT_BUDGET,
        provider.create_invoice(
            batch.id,
            &InvoiceRequest {
                contact_name: batch.contact_name.clone(),
                contact_email: batch.contact_email.clone(),
                reference: batch.reference.clone(),
                line_items: line_items.to_vec(),
                due_date: None,
            },
        ),
    )
    .await
    .map_err(|_| billing::BillingError::Provider("invoice creation timed out".into()))??;
    let attached = billing_batches::attach_invoice(pg, batch.id, &invoice.0).await?;
    Ok((invoice, attached.updated_at))
}

/// Raise the invoice for a persisted batch — fresh or resumed — and settle
/// it: mirror, mark, link, complete. The provider call is keyed on the
/// batch's own id within a bounded retry window; a saved invoice id resumes
/// local settlement without another create or contact call. The debtor and
/// the amounts stay frozen in the batch.
async fn raise_batch(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    project_id: Uuid,
    batch: &billing_batches::MatterBillingBatch,
    actor_id: Uuid,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    // The frozen request comes off the batch, never off the live pool: the
    // retry presents exactly what the first attempt presented.
    let line_items: Vec<InvoiceLine> = serde_json::from_value(batch.line_items.clone())?;
    // A batch that already holds the provider's invoice id re-raises
    // nothing — the saved id IS the invoice. A fresh raise saves the id the
    // moment it returns, before any local flip, so a crash or refusal
    // mid-settle leaves the resume holding the exact invoice instead of
    // relying on the provider's own idempotency window.
    let (invoice_id, issued_at) = invoice_for_batch(pg, provider, batch, &line_items).await?;

    let invoice_state = provider.get_invoice(&invoice_id.0).await?;
    let existing = xero_invoices::for_projects(surreal, &[project_id])
        .await
        .map_err(Box::new)?
        .into_iter()
        .find(|invoice| invoice.xero_invoice_id == invoice_id.0);
    let issued_at = existing
        .as_ref()
        .map_or(issued_at, |invoice| invoice.issued_at);
    let due_at = existing.and_then(|invoice| invoice.due_at);
    // Mirror first (the costs' link validates against it), then flip the
    // consumed work through its own audited transitions — reserved rows
    // included. A refusal anywhere below leaves the earlier local steps
    // describing what Xero genuinely holds, and every flip is replay-safe
    // under the same invoice id: the retry resumes this batch.
    xero_invoices::upsert(
        surreal,
        &xero_invoices::UpsertXeroInvoice {
            project_id,
            xero_invoice_id: invoice_id.0.clone(),
            reference: batch.reference.clone(),
            status: invoice_state.status.clone(),
            amount_cents: batch.total_cents,
            currency: batch.currency.clone(),
            issued_at,
            due_at,
        },
    )
    .await
    .map_err(Box::new)?;
    xero_invoices::record_reconcile(
        surreal,
        &invoice_id.0,
        &invoice_state.status,
        invoice_state.amount_paid_cents,
    )
    .await
    .map_err(Box::new)?;
    if !matches!(invoice_state.status.as_str(), "AUTHORISED" | "PAID") {
        return Err(InvoiceGenerationError::InvoiceNotBillable);
    }
    if !batch.entry_ids.is_empty() {
        time_entries::mark_invoiced(pg, &batch.entry_ids, actor_id, &invoice_id.0, true).await?;
    }
    for cost_id in &batch.cost_ids {
        costs::link_invoice(surreal, pg, *cost_id, actor_id, &invoice_id.0, true).await?;
    }
    billing_batches::complete(pg, batch.id).await?;

    Ok(InvoiceReceipt {
        xero_invoice_id: invoice_id.0,
        reference: batch.reference.clone(),
        currency: batch.currency.clone(),
        total_cents: batch.total_cents,
        line_items,
        consumed_time_entries: batch.entry_ids.len(),
        consumed_costs: batch.cost_ids.len(),
        flat_fee: batch.flat_fee_cents.is_some(),
    })
}

fn refused(status: StatusCode, reason: &'static str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": "invoice_generation_refused",
            "reason": reason,
        })),
    )
        .into_response()
}

/// Map a generation refusal to its response: a 5xx logs first — the store or
/// the provider failed and the matter's state is untouched — while a 4xx is
/// the matter's own state speaking.
fn error(error: &InvoiceGenerationError, project_id: Uuid) -> Response {
    let (status, reason) = match error {
        InvoiceGenerationError::NothingToInvoice => (StatusCode::CONFLICT, "nothing_to_invoice"),
        InvoiceGenerationError::FlatFeeAlreadyBilled
        | InvoiceGenerationError::Batch(billing_batches::BillingBatchError::FlatFeeAlreadyBilled) => {
            (StatusCode::CONFLICT, "flat_fee_already_billed")
        }
        InvoiceGenerationError::ReconciliationRequired
        | InvoiceGenerationError::InvoiceNotBillable => {
            (StatusCode::CONFLICT, "invoice_reconciliation_required")
        }
        InvoiceGenerationError::CurrencyMismatch(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "currency_mismatch")
        }
        InvoiceGenerationError::NoBilledParty => {
            (StatusCode::UNPROCESSABLE_ENTITY, "no_billed_party")
        }
        InvoiceGenerationError::FutureDatedCost(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "future_dated_cost")
        }
        InvoiceGenerationError::NoMatter => (StatusCode::NOT_FOUND, "no_matter"),
        InvoiceGenerationError::Time(time_entries::TimeEntryError::Conflict)
        | InvoiceGenerationError::Cost(costs::ProjectCostError::Conflict) => {
            (StatusCode::CONFLICT, "work_moved_under_another_invoice")
        }
        InvoiceGenerationError::Billing(billing::BillingError::Provider(_)) => {
            // The provider refused: the caller retries the same command. The
            // failure names the upstream in the log, never in the response.
            tracing::warn!(
                event = "invoice_generation_provider_failed",
                outcome = "provider_error",
                project_id = %project_id,
                store_error = %error
            );
            return refused(StatusCode::BAD_GATEWAY, "provider_refused");
        }
        InvoiceGenerationError::Time(
            time_entries::TimeEntryError::Invalid(_) | time_entries::TimeEntryError::NotParticipant,
        )
        | InvoiceGenerationError::Cost(costs::ProjectCostError::Invalid(_)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_work")
        }
        InvoiceGenerationError::Time(time_entries::TimeEntryError::NotFound)
        | InvoiceGenerationError::Cost(costs::ProjectCostError::NotFound) => {
            (StatusCode::NOT_FOUND, "work_not_found")
        }
        // Store outages and every remaining store fault: the generation is
        // down, not refused.
        InvoiceGenerationError::Time(_)
        | InvoiceGenerationError::Fee(_)
        | InvoiceGenerationError::Cost(_)
        | InvoiceGenerationError::Mirror(_)
        | InvoiceGenerationError::Project(_)
        | InvoiceGenerationError::Person(_)
        | InvoiceGenerationError::Batch(_)
        | InvoiceGenerationError::Json(_) => {
            tracing::warn!(
                event = "invoice_generation_store_failed",
                outcome = "store_error",
                project_id = %project_id,
                store_error = %error
            );
            return refused(StatusCode::SERVICE_UNAVAILABLE, "store_error");
        }
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "invoice_generation_failed",
            outcome = "store_error",
            project_id = %project_id,
            store_error = %error
        );
    }
    refused(status, reason)
}

async fn postgres(
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "invoice_generation_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id
        );
        refused(StatusCode::SERVICE_UNAVAILABLE, "postgres_unavailable")
    })
}

/// Open the billing tables with their dependency rows mirrored — the
/// generation checks the parent Project row and the participants in
/// PostgreSQL, so a matter or participant added before the mirror was enabled
/// would otherwise be wrongly refused there. The same preparation
/// [`crate::time_tracking_api`] applies to its doors.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    let pg = postgres(operation, project_id).await?;
    let mirrored = async {
        postgres_mirror::mirror(
            &state.surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await?;
        let participations = projects::participations_for_project(&state.surreal, project_id)
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
            event = "invoice_generation_mirror_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %store_error
        );
        return Err(refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "postgres_unavailable",
        ));
    }
    Ok(pg)
}

/// `POST /app/api/projects/{id}/invoices` — generate the matter's invoice
/// from its unbilled work.
pub(crate) async fn generate(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer =
        store::access::matter_viewer(&state.surreal, session.person_id, session.role, project_id)
            .await
            .map_err(ApiError::Db)?;
    if !viewer.is_some_and(store::access::MatterViewer::is_firm_side) {
        return Err(ApiError::NotFound);
    }
    let pg = match prepared_store(&state, "generate", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match generate_invoice(
        &state.surreal,
        &pg,
        state.billing_provider.as_ref(),
        project_id,
        actor_id,
        Utc::now(),
    )
    .await
    {
        Ok(receipt) => Ok(Json(receipt).into_response()),
        Err(e) => Ok(error(&e, project_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::{create_window_open, CREATE_ATTEMPT_BUDGET};
    use chrono::{Duration, Utc};

    #[test]
    fn provider_attempt_budget_leaves_a_margin_before_key_expiry() {
        let inserted = Utc::now();
        let last_safe_start = inserted + Duration::minutes(4) - Duration::nanoseconds(1);
        assert!(create_window_open(inserted, last_safe_start));
        let completion = last_safe_start + Duration::from_std(CREATE_ATTEMPT_BUDGET).unwrap();
        assert!(completion <= inserted + Duration::minutes(6) - Duration::minutes(1));
        assert!(!create_window_open(
            inserted,
            inserted + Duration::minutes(4)
        ));
        assert!(!create_window_open(
            inserted,
            inserted - Duration::seconds(1)
        ));
    }
}
