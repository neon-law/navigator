//! Matter-level invoice generation: a matter's unbilled work becomes one
//! draft Stripe invoice (ENG-1342), collected into the matter's trust
//! account or, by a lawyer's recorded approval, the Firm's Operating account
//! (ENG-1343).
//!
//! One command composes a matter's unbilled work: unbilled billable Time
//! Entries at their snapshotted rates plus unbilled confirmed Project Costs,
//! or — when the matter carries an agreed flat fee — the fee itself plus
//! those costs. The composed request and the rows it consumes are persisted
//! as a **billing batch**, their rows reserved, before any provider call.
//! Every Stripe write carries `{batch}/{step}` as its idempotency key, so a
//! retry within Stripe's 24-hour key window resumes the same draft; an older
//! uncertain attempt needs reconciliation. The draft is never finalized or
//! sent: the lawyer reviews and sends it.
//!
//! The draft becomes a Matter Invoice (`store::matter_invoices`), and the
//! consumed entries and costs move to `invoiced` under that ledger row
//! through their own audited transitions. The billed Person's Stripe
//! Customer is reused per Stripe account (`store::stripe_customers`).
//!
//! **Where the money lands.** The matter's state IOLTA, else the Firm's
//! Nevada IOLTA where that is permitted (`store::firm_secrets::stripe_trust_route`).
//! A missing trust route never selects Operating. The matter's responsible
//! lawyer may approve Operating for one invoice of earned work with a
//! reason; the approval is the audit record and the batch is pinned to it.
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule); the matter-scope question is asked once
//! through [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row gets the non-disclosing 404.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use store::{
    attorney_rates, billing_batches, invoice_destinations, matter_invoices, project_costs as costs,
    projects, time_entries,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::billing::{self, InvoiceLine, InvoiceRequest};

/// The currency a generated invoice bills in. A confirmed unbilled cost in
/// another currency refuses the whole generation before anything is raised.
const INVOICE_CURRENCY: &str = store::billing_batches::INVOICE_CURRENCY;

/// The revenue line marker the composed request carries.
const REVENUE_ACCOUNT_CODE: &str = "200";

/// Stripe keeps an idempotency key for 24 hours; a draft attempt older than
/// 23 hours needs reconciliation rather than a blind retry.
const STRIPE_KEY_WINDOW_HOURS: i64 = 23;

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
        contact_id: None,
        contact_name: batch.contact_name.clone(),
        contact_email: batch.contact_email.clone(),
        reference: batch.reference.clone(),
        line_items: line_items.to_vec(),
        issued_date: Some(batch.inserted_at.date_naive()),
        due_date: Some(batch.due_date),
    }
}

/// How an invoice names the matter it bills: `Matter <code>`, the reference
/// trust deposits and withdrawals resolve.
pub(crate) fn matter_reference(code: &str) -> String {
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
pub(crate) async fn billed_party(
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
/// consumed under it, and the Stripe draft and ledger row it became.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InvoiceReceipt {
    pub stripe_invoice_id: String,
    pub matter_invoice_id: Uuid,
    /// `true` for a trust draft, `false` for an approved Operating draft.
    pub stripe_iolta: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination_approval_id: Option<Uuid>,
    pub reference: String,
    pub currency: String,
    pub total_cents: i64,
    pub line_items: Vec<InvoiceLine>,
    /// Time Entries consumed under the invoice. On a flat-fee matter these
    /// are covered by the fee line, not itemized on it.
    pub consumed_time_entries: usize,
    pub consumed_costs: usize,
    /// Whether the invoice billed the matter's agreed flat fee.
    pub flat_fee: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvoiceGenerationError {
    #[error("the Firm has no usable Stripe billing credential for this route")]
    StripeUnavailable,
    #[error(transparent)]
    Destination(#[from] invoice_destinations::DestinationError),
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
    Ledger(#[from] Box<matter_invoices::MatterInvoiceError>),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error(transparent)]
    Person(#[from] store::persons::PersonError),
    #[error("Stripe Customer storage failed")]
    CustomerStore,
}

impl From<matter_invoices::MatterInvoiceError> for InvoiceGenerationError {
    fn from(error: matter_invoices::MatterInvoiceError) -> Self {
        Self::Ledger(Box::new(error))
    }
}

/// What the confirming lawyer asked for beyond the compose itself.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerateInput {
    /// The digest of the compose the lawyer reviewed.
    #[serde(default)]
    pub preview_digest: Option<String>,
    /// Collect this invoice into the Firm's Operating account, for this
    /// reason. Only the matter's responsible lawyer may, and only for
    /// earned work.
    #[serde(default)]
    pub operating_reason: Option<String>,
}

/// The reviewed destination: the digest the lawyer saw, bound to the trust
/// route when one exists, or to "no route" when none does.
fn bind_destination(
    digest: String,
    route: Option<&store::firm_secrets::TrustRoute>,
    firm: Uuid,
) -> String {
    let digest =
        billing_batches::invoice_destination_digest(digest, Some(route.is_some()), Some(firm));
    match route {
        Some(route) => route.bind_preview(&digest),
        None => digest,
    }
}

pub(crate) async fn stripe_client<'a>(
    key: &'a store::firm_secrets::ResolvedCredential,
    route: Option<&store::firm_secrets::TrustRoute>,
) -> Result<billing::stripe::StripeInvoiceClient<'a>, InvoiceGenerationError> {
    let live = store::firm_secrets::stripe_mode()
        .map_err(|_| InvoiceGenerationError::StripeUnavailable)?
        == store::firm_secrets::Mode::Live;
    let client = billing::stripe::StripeInvoiceClient::new(key.expose_for_provider_call(), live)?;
    Ok(match route {
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

/// Generate the matter's invoice as one Stripe draft. `now` is the
/// reservation timestamp and cost-date cutoff; the caller's authorization
/// seam governs who may call, and `responsible` says whether the caller is
/// the matter's responsible lawyer.
///
/// # Errors
///
/// [`InvoiceGenerationError`] when the matter has nothing unbilled, names no
/// client, has no permitted destination, carries a cost in another currency
/// or dated after the invoice, or the store or Stripe refuses. Refusals
/// before the batch is persisted change nothing; afterwards the work stays
/// reserved under the pending batch and a retry resumes it.
#[allow(clippy::too_many_lines)] // Destination, reservation, and retry ordering stay together.
pub(crate) async fn generate_invoice(
    application_store: &store::application_store::ApplicationStore,
    pg: &store::postgres::PostgresDb,
    kms: &dyn cloud::RuntimeKms,
    project_id: Uuid,
    (actor_id, responsible): (Uuid, bool),
    now: DateTime<Utc>,
    input: &GenerateInput,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    let project = projects::find_by_id(application_store, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;
    let firm = project
        .firm_id
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let route = store::firm_secrets::stripe_trust_route(application_store, &project)
        .await
        .map_err(|_| InvoiceGenerationError::StripeUnavailable)?;

    // A pending batch is the matter's open invoice intent: the retry resumes
    // that exact batch, pinned to the destination it was created with.
    if let Some(batch) = billing_batches::pending_for_project(pg, project_id).await? {
        let operating = batch.destination_approval_id.is_some();
        if batch.stripe_account_id.is_none()
            || (!operating
                && !store::firm_secrets::stripe_batch_matches_route(&batch, route.as_ref()))
        {
            return Err(InvoiceGenerationError::ReconciliationRequired);
        }
        let route = route.filter(|_| !operating);
        let kind = route.as_ref().map_or(
            store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
            store::firm_secrets::TrustRoute::credential_kind,
        );
        let key = stripe_credential(application_store, kms, firm, kind)
            .await?
            .ok_or(InvoiceGenerationError::StripeUnavailable)?;
        let stripe = stripe_client(&key, route.as_ref()).await?;
        return raise_stripe_draft(pg, &stripe, firm, &batch, actor_id).await;
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
    if flat_fee_cents.is_none() && entries.is_empty() && unbilled_costs.is_empty() {
        return Err(InvoiceGenerationError::NothingToInvoice);
    }
    // Under a consumed fee agreement the matter's work is covered by the
    // fee: billing later time hourly would charge the client twice.
    if fee_billed && unbilled_costs.is_empty() {
        return Err(InvoiceGenerationError::FlatFeeAlreadyBilled);
    }
    let entries = if fee_billed { Vec::new() } else { entries };
    let today = now.date_naive();
    if let Some(cost) = unbilled_costs.iter().find(|cost| cost.occurred_on > today) {
        return Err(InvoiceGenerationError::FutureDatedCost(cost.occurred_on));
    }
    let compose = compose_lines(&entries, &unbilled_costs, flat_fee_cents)?;
    if compose.total_cents == 0 {
        return Err(InvoiceGenerationError::NothingToInvoice);
    }
    // The confirm step's binding: the digest the lawyer reviewed must be the
    // digest of THIS compose and the destination it was shown.
    let preview_digest = bind_destination(
        billing_batches::preview_digest(&entries, &unbilled_costs, flat_fee_cents),
        route.as_ref(),
        firm,
    );
    if input
        .preview_digest
        .as_deref()
        .is_some_and(|expected| expected != preview_digest)
    {
        return Err(InvoiceGenerationError::PreviewChanged);
    }
    // A flat fee may be an advance for work not yet done; unearned money
    // collects into trust, never Operating.
    let operating =
        input
            .operating_reason
            .as_deref()
            .map(|reason| invoice_destinations::OperatingRequest {
                reason: reason.to_owned(),
            });
    let route = invoice_destinations::choose(
        route,
        operating.as_ref(),
        responsible,
        flat_fee_cents.is_some(),
    )?;
    let kind = route.as_ref().map_or(
        store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
        store::firm_secrets::TrustRoute::credential_kind,
    );
    let key = stripe_credential(application_store, kms, firm, kind)
        .await?
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let stripe = stripe_client(&key, route.as_ref()).await?;

    let Some(party) = billed_party(application_store, project_id).await? else {
        return Err(InvoiceGenerationError::NoBilledParty);
    };
    let due_date = matter_due_date(today)?;
    let batch = billing_batches::insert_with_reservations(
        pg,
        &billing_batches::MatterBillingBatch {
            id: Uuid::new_v4(),
            project_id,
            status: "pending".to_string(),
            reference: matter_reference(&project.code),
            contact_name: party.name.clone(),
            contact_email: party.email.clone(),
            billed_person_id: Some(party.id),
            stripe_account_id: Some(stripe.account_id().await?),
            trust_state: route.as_ref().map(|route| route.state.clone()),
            stripe_iolta: route.is_some(),
            stripe_invoice_id: None,
            due_date,
            first_attempted_at: None,
            line_items: serde_json::to_value(&compose.line_items)?,
            total_cents: compose.total_cents,
            currency: INVOICE_CURRENCY.to_string(),
            entry_ids: entries.iter().map(|entry| entry.id).collect(),
            cost_ids: unbilled_costs.iter().map(|cost| cost.id).collect(),
            matter_invoice_id: None,
            destination_approval_id: None,
            flat_fee_cents,
            actor_id,
            inserted_at: now,
            updated_at: now,
        },
        agreed_fee_cents.map(|_| billed_fee_cents),
        operating
            .as_ref()
            .map(|request| billing_batches::OperatingApproval {
                firm_id: firm,
                reason: request.reason.trim(),
                preview_digest: &preview_digest,
            })
            .as_ref(),
    )
    .await?;
    raise_stripe_draft(pg, &stripe, firm, &batch, actor_id).await
}

pub(crate) async fn stripe_credential(
    application_store: &store::application_store::ApplicationStore,
    kms: &dyn cloud::RuntimeKms,
    firm: Uuid,
    kind: store::firm_secrets::IntegrationSecretKind,
) -> Result<Option<store::firm_secrets::ResolvedCredential>, InvoiceGenerationError> {
    use store::firm_secrets::{IntegrationProvider, SecretStoreError};
    if !store::firm_secrets::stripe_billing_enabled(kind) {
        return Ok(None);
    }
    match store::firm_secrets::resolve_firm_credential(
        application_store,
        firm,
        IntegrationProvider::Stripe,
        kind,
        kms,
    )
    .await
    {
        Ok(key) if !key.expose_for_provider_call().trim().is_empty() => Ok(Some(key)),
        Ok(_) | Err(SecretStoreError::NotConfigured { .. }) => Ok(None),
        Err(_) => Err(InvoiceGenerationError::StripeUnavailable),
    }
}

/// Create (or resume) the batch's Stripe draft, record it in the ledger,
/// and settle the batch: the consumed work moves to `invoiced` under the
/// ledger row. Every step replays safely.
async fn raise_stripe_draft(
    pg: &store::postgres::PostgresDb,
    stripe: &billing::stripe::StripeInvoiceClient<'_>,
    firm: Uuid,
    batch: &billing_batches::MatterBillingBatch,
    actor_id: Uuid,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    let account = stripe.account_id().await?;
    if Some(&account) != batch.stripe_account_id.as_ref() {
        return Err(InvoiceGenerationError::ReconciliationRequired);
    }
    let person = batch
        .billed_person_id
        .ok_or(InvoiceGenerationError::ReconciliationRequired)?;
    let line_items: Vec<InvoiceLine> = serde_json::from_value(batch.line_items.clone())?;
    let ledger = matter_invoices::insert_pending(&matter_invoices::DraftPlan {
        project_id: batch.project_id,
        request_id: batch.id,
        reference: batch.reference.clone(),
        amount_cents: batch.total_cents,
        currency: batch.currency.clone(),
        issued_on: batch.inserted_at.date_naive(),
        due_on: batch.due_date,
        memo: None,
        composition: serde_json::json!({ "line_items": batch.line_items }),
        destination: if batch.stripe_iolta {
            "trust"
        } else {
            "operating"
        },
        trust_state: batch.trust_state.clone(),
        stripe_account_id: account.clone(),
        destination_approval_id: batch.destination_approval_id,
        billed_person_id: person,
        created_by: batch.actor_id,
    })
    .await?;
    let batch = billing_batches::start_attempt(pg, batch.id, Utc::now()).await?;
    let stripe_id = if let Some(id) = &batch.stripe_invoice_id {
        id.clone()
    } else {
        if batch.first_attempted_at.is_some_and(|first| {
            Utc::now() >= first + chrono::TimeDelta::hours(STRIPE_KEY_WINDOW_HOURS)
        }) {
            return Err(InvoiceGenerationError::ReconciliationRequired);
        }
        let customer = stripe_customer(
            pg,
            stripe,
            firm,
            &account,
            (person, &batch.contact_name, &batch.contact_email),
            batch.id,
        )
        .await?;
        let request = request_for_batch(&batch, &line_items);
        let id = stripe.create_draft(batch.id, &request, &customer).await?;
        billing_batches::attach_stripe_draft(pg, batch.id, &id.0).await?;
        id.0
    };
    let ledger = matter_invoices::attach_stripe(batch.project_id, ledger.id, &stripe_id).await?;
    if !batch.entry_ids.is_empty() {
        time_entries::mark_invoiced(pg, &batch.entry_ids, actor_id, ledger.id, true).await?;
    }
    for cost_id in &batch.cost_ids {
        costs::link_invoice(
            &store::application_store::ApplicationStore::uninitialized(),
            pg,
            *cost_id,
            actor_id,
            ledger.id,
            true,
        )
        .await?;
    }
    billing_batches::settle(pg, batch.id, ledger.id).await?;
    Ok(InvoiceReceipt {
        stripe_invoice_id: stripe_id,
        matter_invoice_id: ledger.id,
        stripe_iolta: batch.stripe_iolta,
        destination_approval_id: batch.destination_approval_id,
        reference: batch.reference.clone(),
        currency: batch.currency.clone(),
        total_cents: batch.total_cents,
        line_items,
        consumed_time_entries: batch.entry_ids.len(),
        consumed_costs: batch.cost_ids.len(),
        flat_fee: batch.flat_fee_cents.is_some(),
    })
}

/// The billed Person's Stripe Customer on `account`: the saved one while it
/// still exists, else one created under `key` and saved for the next invoice.
pub(crate) async fn stripe_customer(
    pg: &store::postgres::PostgresDb,
    stripe: &billing::stripe::StripeInvoiceClient<'_>,
    firm: Uuid,
    account: &str,
    (person, name, email): (Uuid, &str, &str),
    key: Uuid,
) -> Result<String, InvoiceGenerationError> {
    let saved = store::stripe_customers::find(pg, firm, account, person)
        .await
        .map_err(|_| InvoiceGenerationError::CustomerStore)?;
    let customer = stripe
        .ensure_customer(saved.as_deref(), key, name, email)
        .await?;
    if saved.as_deref() != Some(customer.as_str()) {
        store::stripe_customers::save(pg, firm, account, person, &customer)
            .await
            .map_err(|_| InvoiceGenerationError::CustomerStore)?;
    }
    Ok(customer)
}

/// The billing pool, for a caller that answers with its own refusal.
pub(crate) async fn postgres_db() -> Result<store::postgres::PostgresDb, InvoiceGenerationError> {
    store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| InvoiceGenerationError::CustomerStore)
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
pub(crate) fn error(error: &InvoiceGenerationError, project_id: Uuid) -> Response {
    use invoice_destinations::DestinationError;
    let (status, reason) = match error {
        InvoiceGenerationError::StripeUnavailable => {
            (StatusCode::SERVICE_UNAVAILABLE, "stripe_credential_missing")
        }
        InvoiceGenerationError::Destination(DestinationError::TrustRouteMissing) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "trust_route_missing")
        }
        InvoiceGenerationError::Destination(DestinationError::NotResponsibleLawyer) => (
            StatusCode::FORBIDDEN,
            "operating_approval_needs_the_responsible_lawyer",
        ),
        InvoiceGenerationError::Destination(DestinationError::NotEligible) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "unearned_fees_collect_into_trust",
        ),
        InvoiceGenerationError::Destination(DestinationError::Invalid(_)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "operating_reason_required",
        ),
        InvoiceGenerationError::NothingToInvoice => (StatusCode::CONFLICT, "nothing_to_invoice"),
        InvoiceGenerationError::PreviewChanged => {
            (StatusCode::CONFLICT, "compose_changed_under_review")
        }
        InvoiceGenerationError::FlatFeeAlreadyBilled
        | InvoiceGenerationError::Batch(billing_batches::BillingBatchError::FlatFeeAlreadyBilled) => {
            (StatusCode::CONFLICT, "flat_fee_already_billed")
        }
        InvoiceGenerationError::ReconciliationRequired => {
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
            // Stripe refused: the caller retries the same command. The
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
        _ => {
            tracing::warn!(
                event = "invoice_generation_store_failed",
                outcome = "store_error",
                project_id = %project_id,
                store_error = %error
            );
            return refused(StatusCode::SERVICE_UNAVAILABLE, "store_error");
        }
    };
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

/// `POST /app/api/projects/{id}/invoices` — draft the matter's invoice in
/// Stripe from its unbilled work.
pub(crate) async fn generate(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    axum::Extension(kms): axum::Extension<std::sync::Arc<dyn cloud::RuntimeKms>>,
    Path(project_id): Path<Uuid>,
    input: Option<Json<GenerateInput>>,
) -> Result<Response, ApiError> {
    if session.viewing_as_dri.is_some() {
        return Err(ApiError::Forbidden);
    }
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer = store::access::matter_viewer(
        &state.application_store,
        session.person_id,
        session.role,
        project_id,
    )
    .await
    .map_err(ApiError::Db)?
    .filter(|viewer| viewer.is_firm_side())
    .ok_or(ApiError::NotFound)?;
    let pg = match postgres("generate", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    let input = input.map(|Json(input)| input).unwrap_or_default();
    match generate_invoice(
        &state.application_store,
        &pg,
        kms.as_ref(),
        project_id,
        (actor_id, viewer == store::access::MatterViewer::LawyerDri),
        Utc::now(),
        &input,
    )
    .await
    {
        Ok(receipt) => Ok(Json(receipt).into_response()),
        Err(e) => Ok(error(&e, project_id)),
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
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
            billed_person_id: None,
            stripe_account_id: Some("acct_synthetic".into()),
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
            matter_invoice_id: None,
            destination_approval_id: None,
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
