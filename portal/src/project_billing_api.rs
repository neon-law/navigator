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
    attorney_rates, billing_batches, project_costs as costs, projects, time_entries, xero_invoices,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::billing::{self, InvoiceLine, InvoiceRequest};

/// The currency a generated invoice bills in — the deployment's receivables
/// default, the same one the mirror's ingest falls back to. A confirmed
/// unbilled cost in another currency refuses the whole generation (before
/// anything is raised) rather than silently dropping reimbursable money or
/// mixing currencies on one Xero invoice.
const INVOICE_CURRENCY: &str = store::billing_batches::INVOICE_CURRENCY;

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

/// The matter invoice term: fourteen calendar days from its frozen issue date.
fn matter_due_date(issued: NaiveDate) -> Result<NaiveDate, InvoiceGenerationError> {
    issued
        .checked_add_days(chrono::Days::new(14))
        .ok_or_else(|| billing_batches::BillingBatchError::Invalid("invoice due date").into())
}

fn request_for_batch(
    batch: &billing_batches::MatterBillingBatch,
    line_items: &[InvoiceLine],
) -> InvoiceRequest {
    InvoiceRequest {
        contact_id: batch.xero_contact_id.clone().map(billing::ContactId),
        contact_name: batch.contact_name.clone(),
        contact_email: batch.contact_email.clone(),
        reference: batch.reference.clone(),
        line_items: line_items.to_vec(),
        issued_date: Some(batch.inserted_at.date_naive()),
        due_date: Some(batch.due_date),
    }
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
    application_store: &store::application_store::ApplicationStore,
    project_id: Uuid,
) -> Result<Option<store::persons::Person>, InvoiceGenerationError> {
    let participations =
        projects::participations_for_project(application_store, project_id).await?;
    // Access scopes include legacy counterparties; only a matter's actual
    // client participation is a debtor. DRI flags never promote another side.
    let clients = participations
        .into_iter()
        .filter(|row| row.participation == "client")
        .collect::<Vec<_>>();
    let party = clients
        .iter()
        .find(|row| row.is_client_dri)
        .or_else(|| clients.first());
    match party {
        Some(row) => Ok(store::persons::find_by_id(application_store, row.person_id).await?),
        None => Ok(None),
    }
}

/// One raised invoice as the command answers it: what was billed, the parts
/// that were consumed under it, and the Xero id every local flip carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InvoiceReceipt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xero_invoice_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stripe_invoice_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stripe_iolta: Option<bool>,
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
    #[error("invoice generation is unavailable without a billing provider")]
    BillingUnavailable,
    #[error("the Firm has no usable Stripe billing credential for this route")]
    StripeUnavailable,
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
    #[error("the compose changed since the preview — review it again")]
    PreviewChanged,
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
// The shared command keeps provider selection, reservation, and retry ordering together.
#[allow(clippy::too_many_lines)]
pub(crate) async fn generate_invoice(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    project_id: Uuid,
    actor_id: Uuid,
    now: DateTime<Utc>,
    expected_preview_digest: Option<&str>,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    let project = projects::find_by_id(application_store, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;

    // A pending batch is the matter's open invoice intent: its request and
    // rows were frozen (and reserved) when this command last ran, so the
    // retry resumes that exact batch instead of recomposing a different one —
    // new work logged meanwhile composes the NEXT batch, it never slips into
    // this one.
    let pending = billing_batches::pending_for_project(pg, project_id).await?;
    if let Some(batch) = pending
        .as_ref()
        .filter(|batch| batch.stripe_account_id.is_none())
    {
        if !provider.invoices_available() {
            return Err(InvoiceGenerationError::BillingUnavailable);
        }
        return raise_batch(application_store, pg, provider, project_id, batch, actor_id).await;
    }
    let route = store::firm_secrets::stripe_trust_route(application_store, &project)
        .await
        .map_err(|_| InvoiceGenerationError::StripeUnavailable)?;
    if pending.as_ref().is_some_and(|batch| {
        !store::firm_secrets::stripe_batch_matches_route(batch, route.as_ref())
    }) {
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    let kind = route.as_ref().map_or(
        store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
        store::firm_secrets::TrustRoute::credential_kind,
    );
    let stripe_key = stripe_credential(application_store, project.firm_id, kind).await?;
    let stripe = match &stripe_key {
        Some(key) => {
            let live = store::firm_secrets::stripe_mode()
                .map_err(|_| InvoiceGenerationError::StripeUnavailable)?
                == store::firm_secrets::Mode::Live;
            let client =
                billing::stripe::StripeInvoiceClient::new(key.expose_for_provider_call(), live)?;
            Some(match &route {
                Some(route) if route.dedicated_nevada => {
                    if client.account_id().await? != route.account {
                        return Err(InvoiceGenerationError::StripeUnavailable);
                    }
                    client
                }
                Some(route) => client.on_connected_trust(&route.account).await?,
                None => client,
            })
        }
        None => None,
    };
    let stripe_iolta = stripe.as_ref().map(|_| route.is_some());
    if (route.is_some() || pending.is_some()) && stripe.is_none() {
        return Err(InvoiceGenerationError::StripeUnavailable);
    }
    if let (Some(batch), Some(stripe)) = (&pending, &stripe) {
        return raise_stripe_draft(pg, stripe, batch).await;
    }
    if stripe.is_none() && !provider.invoices_available() {
        return Err(InvoiceGenerationError::BillingUnavailable);
    }

    let costs_only = store::billing_batches::costs_only(pg, project_id, now.date_naive()).await?;
    let entries = time_entries::unbilled_billable(pg, project_id).await?;
    let unbilled_costs = costs::list_for_project(pg, project_id)
        .await?
        .into_iter()
        .filter(|cost| cost.amount_status == "confirmed" && cost.billing_status == "unbilled")
        .collect::<Vec<_>>();
    let agreed_fee_cents = attorney_rates::flat_fee_cents(pg, project_id).await?;
    let agreed_fee_cents = agreed_fee_cents.filter(|_| !costs_only);
    let entries = if costs_only { Vec::new() } else { entries };
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
    // for the same agreement. Later reimbursable costs remain billable
    // independently; a cost-only batch leaves covered time unreserved.
    if fee_billed && unbilled_costs.is_empty() {
        return Err(InvoiceGenerationError::FlatFeeAlreadyBilled);
    }
    let entries = if fee_billed { Vec::new() } else { entries };
    // A cost dated after the invoice composes fine but would refuse at
    // link_invoice — after Xero already holds the invoice. The same rule
    // runs here, before anything is raised.
    let today = now.date_naive();
    if let Some(cost) = unbilled_costs.iter().find(|cost| cost.occurred_on > today) {
        return Err(InvoiceGenerationError::FutureDatedCost(cost.occurred_on));
    }
    let compose = compose_lines(&entries, &unbilled_costs, flat_fee_cents)?;
    if compose.total_cents == 0 {
        return Err(InvoiceGenerationError::NothingToInvoice);
    }
    // The confirm step's binding: the digest the attorney reviewed must be
    // the digest of THIS fresh compose — work logged or edited since the
    // preview re-opens the review instead of billing charges nobody saw. The
    // pending-batch resume above never recomposes, so the binding cannot
    // strand an already-frozen batch.
    let preview_digest = billing_batches::invoice_destination_digest(
        billing_batches::preview_digest(&entries, &unbilled_costs, flat_fee_cents),
        stripe_iolta,
        project.firm_id,
    );
    let preview_digest = match &route {
        Some(route) => route.bind_preview(&preview_digest),
        None => preview_digest,
    };
    if expected_preview_digest.is_some_and(|expected| expected != preview_digest) {
        return Err(InvoiceGenerationError::PreviewChanged);
    }

    let Some(party) = billed_party(application_store, project_id).await? else {
        return Err(InvoiceGenerationError::NoBilledParty);
    };
    // Resolve the billed party first — the seam's documented order — then
    // reserve the batch's rows and persist the intent BEFORE the provider
    // call. Reservation is what makes the invoice's work final: a reserved
    // entry cannot be written off and a reserved cost cannot be resolved or
    // re-confirmed, so nothing the lawyer removed can slip into the batch
    // while Xero is deciding.
    let due_date = matter_due_date(now.date_naive())?;
    let (contact, stripe_account_id) = if let Some(stripe) = &stripe {
        (None, Some(stripe.account_id().await?))
    } else {
        let contact = provider
            .ensure_contact(&billing::ContactRequest {
                name: party.name.clone(),
                email: party.email.clone(),
            })
            .await?;
        if contact.0.trim().is_empty() {
            return Err(billing::BillingError::Provider(
                "resolved invoice contact id missing".into(),
            )
            .into());
        }
        (Some(contact.0), None)
    };
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
            xero_contact_id: contact,
            stripe_account_id,
            trust_state: route.as_ref().map(|route| route.state.clone()),
            stripe_iolta: stripe_iolta == Some(true),
            stripe_invoice_id: None,
            due_date,
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
    if let Some(stripe) = &stripe {
        raise_stripe_draft(pg, stripe, &batch).await
    } else {
        raise_batch(
            application_store,
            pg,
            provider,
            project_id,
            &batch,
            actor_id,
        )
        .await
    }
}

async fn stripe_credential(
    application_store: &store::application_store::ApplicationStore,
    firm: Option<Uuid>,
    kind: store::firm_secrets::IntegrationSecretKind,
) -> Result<Option<store::firm_secrets::ResolvedCredential>, InvoiceGenerationError> {
    use store::firm_secrets::{IntegrationProvider, SecretStoreError};
    let Some(firm) = firm else {
        return Ok(None);
    };
    if !store::firm_secrets::stripe_billing_enabled(kind) {
        return Ok(None);
    }
    let kms = crate::integrations::runtime_kms_from_env().await;
    match store::firm_secrets::resolve_firm_credential(
        application_store,
        firm,
        IntegrationProvider::Stripe,
        kind,
        kms.as_ref(),
    )
    .await
    {
        Ok(key) if !key.expose_for_provider_call().trim().is_empty() => Ok(Some(key)),
        Ok(_) | Err(SecretStoreError::NotConfigured { .. }) => Ok(None),
        Err(_) => Err(InvoiceGenerationError::StripeUnavailable),
    }
}

async fn raise_stripe_draft(
    pg: &store::postgres::PostgresDb,
    stripe: &billing::stripe::StripeInvoiceClient<'_>,
    batch: &billing_batches::MatterBillingBatch,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    if Some(stripe.account_id().await?) != batch.stripe_account_id {
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    let line_items: Vec<InvoiceLine> = serde_json::from_value(batch.line_items.clone())?;
    let batch = billing_batches::start_attempt(pg, batch.id, Utc::now()).await?;
    let stripe_id = if let Some(id) = &batch.stripe_invoice_id {
        id.clone()
    } else {
        if batch
            .first_attempted_at
            .is_some_and(|first| Utc::now() >= first + chrono::TimeDelta::hours(23))
        {
            return Err(InvoiceGenerationError::ReconciliationRequired);
        }
        let request = request_for_batch(&batch, &line_items);
        let id = stripe.create_trust_draft(batch.id, &request).await?;
        billing_batches::attach_stripe_draft(pg, batch.id, &id.0).await?;
        id.0
    };
    Ok(InvoiceReceipt {
        xero_invoice_id: None,
        stripe_invoice_id: Some(stripe_id),
        stripe_iolta: Some(batch.stripe_iolta),
        reference: batch.reference,
        currency: batch.currency,
        total_cents: batch.total_cents,
        line_items,
        consumed_time_entries: 0,
        consumed_costs: 0,
        flat_fee: batch.flat_fee_cents.is_some(),
    })
}

async fn invoice_for_batch(
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    batch: &billing_batches::MatterBillingBatch,
    line_items: &[InvoiceLine],
) -> Result<billing::InvoiceId, InvoiceGenerationError> {
    if let Some(saved) = &batch.xero_invoice_id {
        return Ok(billing::InvoiceId(saved.clone()));
    }
    if batch
        .first_attempted_at
        .is_some_and(|first| !create_window_open(first, Utc::now()))
    {
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    let attempted = billing_batches::start_attempt(pg, batch.id, Utc::now()).await?;
    if let Some(saved) = attempted.xero_invoice_id {
        return Ok(billing::InvoiceId(saved));
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
    let request = request_for_batch(batch, line_items);
    let invoice = tokio::time::timeout(
        CREATE_ATTEMPT_BUDGET,
        provider.create_invoice(batch.id, &request),
    )
    .await
    .map_err(|_| billing::BillingError::Provider("invoice creation timed out".into()))??;
    billing_batches::attach_invoice(pg, batch.id, &invoice.0).await?;
    Ok(invoice)
}

/// Raise the invoice for a persisted batch — fresh or resumed — and settle
/// it: mirror, mark, link, complete. The provider call is keyed on the
/// batch's own id within a bounded retry window; a saved invoice id resumes
/// local settlement without another create or contact call. The debtor and
/// the amounts stay frozen in the batch.
async fn raise_batch(
    application_store: &store::application_store::ApplicationStore,
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
    let invoice_id = invoice_for_batch(pg, provider, batch, &line_items).await?;

    let invoice_state = provider.get_invoice(&invoice_id.0).await?;
    let amount_cents = invoice_state.amount_cents.ok_or_else(|| {
        billing::BillingError::Provider("invoice total missing from provider read".into())
    })?;
    let currency = invoice_state
        .currency
        .as_ref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            billing::BillingError::Provider("invoice currency missing from provider read".into())
        })?;
    // Provider dates can be corrected after creation. Mirror their current
    // values; attachment and retry clocks never redate the receivables ledger.
    // https://developer.xero.com/documentation/api/accounting/invoices
    let issued_at = invoice_state.issued_at.ok_or_else(|| {
        billing::BillingError::Provider("invoice issued date missing or malformed".into())
    })?;
    let due_at = invoice_state.due_at;
    // Mirror first (the costs' link validates against it), then flip the
    // consumed work through its own audited transitions — reserved rows
    // included. A refusal anywhere below leaves the earlier local steps
    // describing what Xero genuinely holds, and every flip is replay-safe
    // under the same invoice id: the retry resumes this batch.
    xero_invoices::upsert(
        application_store,
        &xero_invoices::UpsertXeroInvoice {
            project_id,
            xero_invoice_id: invoice_id.0.clone(),
            reference: batch.reference.clone(),
            status: invoice_state.status.clone(),
            amount_cents,
            currency: currency.clone(),
            issued_at,
            due_at,
        },
    )
    .await
    .map_err(Box::new)?;
    xero_invoices::record_reconcile(
        application_store,
        &invoice_id.0,
        &invoice_state.status,
        invoice_state.amount_paid_cents,
    )
    .await
    .map_err(Box::new)?;
    if currency != &batch.currency {
        // Mirror the provider truth, but do not consume work denominated in
        // another currency. A corrected invoice needs manual reconciliation.
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    if !matches!(invoice_state.status.as_str(), "AUTHORISED" | "PAID") {
        return Err(InvoiceGenerationError::InvoiceNotBillable);
    }
    // A provider date corrected to before a reserved cost must not consume
    // time first and only then fail while linking that cost.
    for cost_id in &batch.cost_ids {
        if costs::find_by_id(pg, *cost_id).await?.occurred_on > issued_at.date_naive() {
            return Err(InvoiceGenerationError::ReconciliationRequired);
        }
    }
    if !batch.entry_ids.is_empty() {
        time_entries::mark_invoiced(pg, &batch.entry_ids, actor_id, &invoice_id.0, true).await?;
    }
    for cost_id in &batch.cost_ids {
        costs::link_invoice(
            application_store,
            pg,
            *cost_id,
            actor_id,
            &invoice_id.0,
            true,
        )
        .await?;
    }
    billing_batches::complete(pg, batch.id).await?;

    Ok(InvoiceReceipt {
        xero_invoice_id: Some(invoice_id.0),
        stripe_invoice_id: None,
        stripe_iolta: None,
        reference: batch.reference.clone(),
        currency: currency.clone(),
        total_cents: amount_cents,
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
        InvoiceGenerationError::BillingUnavailable => {
            tracing::warn!(event = "invoice_generation_billing_unavailable", outcome = "unavailable", project_id = %project_id);
            return refused(StatusCode::SERVICE_UNAVAILABLE, "billing_unavailable");
        }
        InvoiceGenerationError::StripeUnavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "stripe_credential_missing")
        }
        InvoiceGenerationError::NothingToInvoice => (StatusCode::CONFLICT, "nothing_to_invoice"),
        InvoiceGenerationError::PreviewChanged => {
            (StatusCode::CONFLICT, "compose_changed_under_review")
        }
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
        | InvoiceGenerationError::Cost(costs::ProjectCostError::Conflict)
        | InvoiceGenerationError::Batch(
            billing_batches::BillingBatchError::PendingExists
            | billing_batches::BillingBatchError::Time(time_entries::TimeEntryError::Conflict)
            | billing_batches::BillingBatchError::Cost(costs::ProjectCostError::Conflict),
        ) => (StatusCode::CONFLICT, "work_moved_under_another_invoice"),
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

/// Open PostgreSQL for the authorized operation.
async fn prepared_store(
    _state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    postgres(operation, project_id).await
}

/// `POST /app/api/projects/{id}/invoices` — generate the matter's invoice
/// from its unbilled work.
pub(crate) async fn generate(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer = store::access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
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
        &state.application_store,
        &pg,
        state.billing_provider.as_ref(),
        project_id,
        actor_id,
        Utc::now(),
        None,
    )
    .await
    {
        Ok(receipt) => Ok(Json(receipt).into_response()),
        Err(e) => Ok(error(&e, project_id)),
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn competing_batch_reservations_are_conflicts() {
        use store::billing_batches::BillingBatchError;
        let failures = [
            BillingBatchError::PendingExists,
            BillingBatchError::Time(store::time_entries::TimeEntryError::Conflict),
            BillingBatchError::Cost(store::project_costs::ProjectCostError::Conflict),
        ];
        for failure in failures {
            let response = super::error(
                &super::InvoiceGenerationError::Batch(failure),
                uuid::Uuid::nil(),
            );
            assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["reason"], "work_moved_under_another_invoice");
        }
        let outage = super::InvoiceGenerationError::Batch(BillingBatchError::Database(
            sqlx::Error::PoolClosed,
        ));
        assert_eq!(
            super::error(&outage, uuid::Uuid::nil()).status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

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

    #[test]
    fn matter_term_is_fourteen_calendar_days_at_date_boundaries() {
        for (issued, due) in [
            ("2026-10-31", "2026-11-14"),
            ("2026-12-25", "2027-01-08"),
            ("2028-02-20", "2028-03-05"),
        ] {
            assert_eq!(
                super::matter_due_date(issued.parse().unwrap()).unwrap(),
                due.parse::<chrono::NaiveDate>().unwrap()
            );
        }
        assert!(super::matter_due_date(chrono::NaiveDate::MAX).is_err());
    }

    #[test]
    fn saved_batch_request_keeps_dates_across_midnight_and_retained_terms() {
        let inserted = "2026-10-31T23:59:59Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let retry_clock = inserted + chrono::Duration::seconds(2);
        let batch = store::billing_batches::MatterBillingBatch {
            id: uuid::Uuid::new_v4(),
            project_id: uuid::Uuid::new_v4(),
            status: "pending".into(),
            reference: "Synthetic invoice".into(),
            contact_name: "Synthetic client".into(),
            contact_email: "client@example.com".into(),
            xero_contact_id: Some("synthetic-resolved-contact".into()),
            stripe_account_id: None,
            trust_state: None,
            stripe_iolta: false,
            stripe_invoice_id: None,
            due_date: "2026-11-14".parse().unwrap(),
            first_attempted_at: None,
            line_items: serde_json::json!([]),
            total_cents: 100,
            currency: "USD".into(),
            entry_ids: vec![],
            cost_ids: vec![],
            xero_invoice_id: None,
            flat_fee_cents: Some(100),
            actor_id: uuid::Uuid::new_v4(),
            inserted_at: inserted,
            updated_at: inserted,
        };
        let request = super::request_for_batch(&batch, &[]);
        assert_eq!(request.issued_date, Some("2026-10-31".parse().unwrap()));
        assert_ne!(request.issued_date, Some(retry_clock.date_naive()));
        assert_eq!(request.due_date, Some("2026-11-14".parse().unwrap()));
        // A retained batch can reflect an earlier term policy. Never
        // recompute its saved deadline using a later default.
        let retained = store::billing_batches::MatterBillingBatch {
            due_date: "2026-11-20".parse().unwrap(),
            ..batch
        };
        assert_eq!(
            super::request_for_batch(&retained, &[]).due_date,
            Some("2026-11-20".parse().unwrap())
        );
    }
}
