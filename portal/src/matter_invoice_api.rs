//! A matter's invoices from the CLI and API (LAW-506, LAW-507): compose a
//! draft Stripe invoice, send it after the lawyer's review, void it or mark
//! it uncollectible with a reason, record a payment, and record money
//! collected outside Stripe.
//!
//! A draft collects into the matter's trust account (ENG-1343) unless the
//! matter's responsible lawyer approves the Firm's Operating account for it
//! with a reason; an advance (a retainer or deposit) always collects into
//! trust. The destination is frozen with the draft and every later step uses
//! it. Nothing reaches the client until `send`.
//!
//! Admission is lawyer tier plus an embedded-Rego `route_allow` rule; the
//! matter question is asked once through [`store::access::matter_viewer`],
//! so a caller without firm-side participation gets the non-disclosing 404.
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use chrono::{NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use store::invoice_destinations::{self, DestinationError, OperatingRequest};
use store::matter_invoices::{self, MatterInvoice, MatterInvoiceError, Source, Status};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::billing::{InvoiceLine, InvoiceRequest};
use crate::project_billing_api::{self as billing_api, InvoiceGenerationError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvoiceError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("the invoice was created with another trust account; reconcile it before going on")]
    DestinationChanged,
    #[error(transparent)]
    Generation(#[from] InvoiceGenerationError),
    #[error(transparent)]
    Ledger(#[from] MatterInvoiceError),
    #[error(transparent)]
    Destination(#[from] DestinationError),
    #[error(transparent)]
    Billing(#[from] crate::billing::BillingError),
}

impl From<store::projects::ProjectStoreError> for InvoiceError {
    fn from(error: store::projects::ProjectStoreError) -> Self {
        Self::Generation(error.into())
    }
}

fn refusal(error: &InvoiceError, operation: &'static str, project: Uuid) -> Response {
    let status = match error {
        InvoiceError::Invalid(_)
        | InvoiceError::Ledger(MatterInvoiceError::Invalid(_))
        | InvoiceError::Destination(
            DestinationError::TrustRouteMissing
            | DestinationError::NotEligible
            | DestinationError::Invalid(_),
        ) => StatusCode::UNPROCESSABLE_ENTITY,
        InvoiceError::Destination(DestinationError::NotResponsibleLawyer) => StatusCode::FORBIDDEN,
        InvoiceError::Ledger(MatterInvoiceError::NotFound) => StatusCode::NOT_FOUND,
        InvoiceError::DestinationChanged
        | InvoiceError::Ledger(MatterInvoiceError::Conflict(_)) => StatusCode::CONFLICT,
        InvoiceError::Generation(inner) => {
            return billing_api::error(inner, project);
        }
        InvoiceError::Billing(_) => StatusCode::BAD_GATEWAY,
        InvoiceError::Destination(DestinationError::Database(_)) | InvoiceError::Ledger(_) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    let message = if status.is_server_error() {
        tracing::warn!(event = "matter_invoice_operation_failed", operation, project_id = %project, error = %error);
        "the invoice could not be written right now".to_owned()
    } else {
        error.to_string()
    };
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

fn reply<T: Serialize>(
    result: Result<T, InvoiceError>,
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

/// The firm-side caller and whether they are the matter's responsible
/// lawyer. Writes refuse an impersonated view.
async fn authorize(
    state: &ApiState,
    session: &crate::session::SessionData,
    project: Uuid,
    write: bool,
) -> Result<(Uuid, bool), ApiError> {
    if write && session.viewing_as_dri.is_some() {
        return Err(ApiError::Forbidden);
    }
    let actor = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer =
        store::access::matter_viewer(&state.application_store, Some(actor), session.role, project)
            .await
            .map_err(ApiError::Db)?
            .filter(|viewer| viewer.is_firm_side())
            .ok_or(ApiError::NotFound)?;
    Ok((actor, viewer == store::access::MatterViewer::LawyerDri))
}

/// `GET .../invoices` — every invoice on the matter, newest first.
pub(crate) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, project, false).await?;
    let invoices = matter_invoices::for_projects(&state.application_store, &[project])
        .await
        .map_err(InvoiceError::from);
    Ok(reply(invoices, false, "list_invoices", project))
}

/// A discount on one line or the whole invoice: a percent in basis points
/// or a fixed amount, always with the reason the client sees.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Discount {
    #[serde(default)]
    percent_bps: Option<u32>,
    #[serde(default)]
    amount_cents: Option<i64>,
    reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LineInput {
    description: String,
    #[serde(default = "one")]
    quantity: u32,
    unit_amount_cents: i64,
    #[serde(default)]
    discount: Option<Discount>,
}

const fn one() -> u32 {
    1
}

/// Credit for a payment already received and accounted for.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Credit {
    amount_cents: i64,
    reason: String,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Terms {
    #[default]
    OnReceipt,
    #[serde(rename = "net_15")]
    Net15,
    #[serde(rename = "net_30")]
    Net30,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Account {
    #[default]
    Trust,
    Operating,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DraftInput {
    request_id: Uuid,
    lines: Vec<LineInput>,
    #[serde(default)]
    terms: Terms,
    /// A due date in place of `terms`.
    #[serde(default)]
    due_on: Option<NaiveDate>,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    discount: Option<Discount>,
    /// A Stripe promotion code the client was given.
    #[serde(default)]
    coupon: Option<String>,
    #[serde(default)]
    credit: Option<Credit>,
    /// A retainer or deposit: unearned money, which collects into trust.
    #[serde(default)]
    advance: bool,
    #[serde(default)]
    account: Account,
    #[serde(default)]
    operating_reason: Option<String>,
}

/// Validate a reason the client sees.
fn reason(text: &str) -> Result<&str, InvoiceError> {
    let text = text.trim();
    if text.is_empty() || text.len() > 300 {
        return Err(InvoiceError::Invalid(
            "a discount or credit needs a reason of 1 to 300 characters",
        ));
    }
    Ok(text)
}

/// How much a discount takes off `base`, never more than `base`.
fn discount_cents(discount: &Discount, base: i64) -> Result<i64, InvoiceError> {
    let off =
        match (discount.percent_bps, discount.amount_cents) {
            (Some(bps), None) if (1..=10_000).contains(&bps) => base * i64::from(bps) / 10_000,
            (None, Some(cents)) if cents > 0 && cents <= base => cents,
            _ => return Err(InvoiceError::Invalid(
                "a discount is a percent from 0.01% to 100% or an amount up to what it discounts",
            )),
        };
    Ok(off)
}

fn line(description: String, cents: i64) -> InvoiceLine {
    InvoiceLine {
        description,
        quantity: 1,
        unit_amount_cents: cents,
        account_code: "200".to_owned(),
    }
}

/// The Stripe lines of a draft: each line, then its discount or write-off,
/// then the invoice discount, then the credit. A balance below zero is
/// refused; a zero balance is a record with nothing to pay.
fn compose(input: &DraftInput) -> Result<(Vec<InvoiceLine>, i64), InvoiceError> {
    if input.lines.is_empty() || input.lines.len() > 100 {
        return Err(InvoiceError::Invalid("an invoice has 1 to 100 lines"));
    }
    let mut lines = Vec::new();
    let mut total = 0i64;
    for item in &input.lines {
        let description = item.description.trim();
        if description.is_empty()
            || description.len() > 500
            || item.quantity == 0
            || item.quantity > 10_000
            || !(1..=100_000_000).contains(&item.unit_amount_cents)
        {
            return Err(InvoiceError::Invalid(
                "a line has a description, a quantity of 1 to 10,000, and a unit price above $0",
            ));
        }
        let amount = item.unit_amount_cents * i64::from(item.quantity);
        let label = if item.quantity == 1 {
            description.to_owned()
        } else {
            format!(
                "{description} ({} × ${}.{:02})",
                item.quantity,
                item.unit_amount_cents / 100,
                item.unit_amount_cents % 100
            )
        };
        lines.push(line(label, amount));
        total += amount;
        if let Some(discount) = &item.discount {
            let off = discount_cents(discount, amount)?;
            let kind = if off == amount {
                "Write-off"
            } else {
                "Discount"
            };
            lines.push(line(
                format!("{kind}: {description} — {}", reason(&discount.reason)?),
                -off,
            ));
            total -= off;
        }
    }
    if let Some(discount) = &input.discount {
        let off = discount_cents(discount, total)?;
        lines.push(line(
            format!("Discount — {}", reason(&discount.reason)?),
            -off,
        ));
        total -= off;
    }
    if let Some(credit) = &input.credit {
        if credit.amount_cents <= 0 || credit.amount_cents > total {
            return Err(InvoiceError::Invalid(
                "a credit is above $0 and no more than the balance",
            ));
        }
        lines.push(line(
            format!("Credit for prior payment — {}", reason(&credit.reason)?),
            -credit.amount_cents,
        ));
        total -= credit.amount_cents;
    }
    Ok((lines, total))
}

fn due_on(input: &DraftInput, today: NaiveDate) -> Result<NaiveDate, InvoiceError> {
    let due = match (input.due_on, input.terms) {
        (Some(day), Terms::OnReceipt) if day >= today => day,
        (Some(_), _) => {
            return Err(InvoiceError::Invalid(
                "name terms or a due date today or later, not both",
            ))
        }
        (None, Terms::OnReceipt) => today,
        (None, Terms::Net15) => today + chrono::Days::new(15),
        (None, Terms::Net30) => today + chrono::Days::new(30),
    };
    Ok(due)
}

/// The draft as the command answers it.
#[derive(Debug, Serialize)]
pub(crate) struct Draft {
    #[serde(flatten)]
    invoice: MatterInvoice,
    line_items: Vec<InvoiceLine>,
    coupon: Option<String>,
}

/// `POST .../invoices/drafts` — compose one draft Stripe invoice.
pub(crate) async fn create(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Extension(kms): Extension<Arc<dyn cloud::RuntimeKms>>,
    Path(project): Path<Uuid>,
    Json(input): Json<DraftInput>,
) -> Result<Response, ApiError> {
    let (actor, responsible) = authorize(&state, &session, project, true).await?;
    let result = draft(&state, kms.as_ref(), project, actor, responsible, &input).await;
    Ok(reply(result, true, "create_invoice", project))
}

/// The memo the client sees, trimmed; at most 1,000 characters.
fn memo(input: &DraftInput) -> Result<Option<&str>, InvoiceError> {
    let memo = input
        .memo
        .as_deref()
        .map(str::trim)
        .filter(|memo| !memo.is_empty());
    if memo.is_some_and(|memo| memo.len() > 1000) {
        return Err(InvoiceError::Invalid("a memo is at most 1,000 characters"));
    }
    Ok(memo)
}

/// Where a draft collects: a replayed request keeps the destination it was
/// frozen with; a new one is chosen from the trust route or an Operating
/// approval. `None` is Operating, with the approval's request.
fn destination(
    existing: Option<&MatterInvoice>,
    input: &DraftInput,
    route: Option<store::firm_secrets::TrustRoute>,
    responsible: bool,
) -> Result<
    (
        Option<store::firm_secrets::TrustRoute>,
        Option<OperatingRequest>,
    ),
    InvoiceError,
> {
    let operating = existing.map_or(input.account == Account::Operating, |invoice| {
        invoice.destination.as_deref() == Some("operating")
    });
    let request = operating.then(|| OperatingRequest {
        reason: input.operating_reason.clone().unwrap_or_default(),
    });
    let route = match (existing, operating) {
        (Some(_), true) => None,
        (Some(_), false) => Some(route.ok_or(DestinationError::TrustRouteMissing)?),
        (None, false) if input.operating_reason.is_some() => {
            return Err(InvoiceError::Invalid(
                "an Operating reason goes with --account operating",
            ))
        }
        (None, _) => {
            invoice_destinations::choose(route, request.as_ref(), responsible, input.advance)?
        }
    };
    Ok((route, request))
}

/// Create the frozen draft in Stripe for the billed Person's Customer,
/// keyed by its ledger row so a retry converges on one draft.
async fn in_stripe(
    stripe: &billing::stripe::StripeInvoiceClient<'_>,
    firm: Uuid,
    account: &str,
    party: &store::persons::Person,
    ledger: &MatterInvoice,
    line_items: &[InvoiceLine],
    (input, memo): (&DraftInput, Option<&str>),
) -> Result<String, InvoiceError> {
    let request = InvoiceRequest {
        contact_id: None,
        contact_name: party.name.clone(),
        contact_email: party.email.clone(),
        reference: ledger.reference.clone(),
        line_items: line_items.to_vec(),
        issued_date: ledger.issued_on,
        due_date: ledger.due_on,
    };
    let pg = billing_api::postgres_db().await?;
    let customer = billing_api::stripe_customer(
        &pg,
        stripe,
        firm,
        account,
        (party.id, &party.name, &party.email),
        ledger.id,
    )
    .await?;
    let draft = billing::stripe::ComposedDraft {
        request: &request,
        customer: &customer,
        promotion_code: input.coupon.as_deref(),
        memo,
    };
    Ok(stripe.create_composed_draft(ledger.id, &draft).await?.0)
}

async fn draft(
    state: &ApiState,
    kms: &dyn cloud::RuntimeKms,
    project_id: Uuid,
    actor: Uuid,
    responsible: bool,
    input: &DraftInput,
) -> Result<Draft, InvoiceError> {
    let (line_items, total) = compose(input)?;
    let today = Utc::now().date_naive();
    let due = due_on(input, today)?;
    let memo = memo(input)?;
    let store = &state.application_store;
    let project = store::projects::find_by_id(store, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;
    let firm = project
        .firm_id
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let route = store::firm_secrets::stripe_trust_route(store, &project)
        .await
        .map_err(|_| InvoiceGenerationError::StripeUnavailable)?;
    let existing = matter_invoices::by_request(project_id, input.request_id).await?;
    let (route, request) = destination(existing.as_ref(), input, route, responsible)?;
    let kind = route.as_ref().map_or(
        store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
        store::firm_secrets::TrustRoute::credential_kind,
    );
    let key = billing_api::stripe_credential(store, kms, firm, kind)
        .await?
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let stripe = billing_api::stripe_client(&key, route.as_ref()).await?;
    let account = stripe.account_id().await?;
    if existing
        .as_ref()
        .is_some_and(|invoice| invoice.stripe_account_id.as_deref() != Some(account.as_str()))
    {
        return Err(InvoiceError::DestinationChanged);
    }
    let party = billing_api::billed_party(store, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoBilledParty)?;
    let composition = serde_json::json!({
        "line_items": line_items, "coupon": input.coupon, "advance": input.advance,
    });
    let plan = matter_invoices::DraftPlan {
        project_id,
        request_id: input.request_id,
        reference: billing_api::matter_reference(&project.code),
        amount_cents: total,
        currency: "USD".to_owned(),
        issued_on: today,
        due_on: due,
        memo: memo.map(str::to_owned),
        composition: composition.clone(),
        destination: if route.is_some() {
            "trust"
        } else {
            "operating"
        },
        trust_state: route.as_ref().map(|route| route.state.clone()),
        stripe_account_id: account.clone(),
        destination_approval_id: None,
        billed_person_id: party.id,
        created_by: actor,
    };
    let ledger = match (&request, &existing) {
        (_, Some(existing)) => existing.clone(),
        (Some(request), None) => {
            let digest = hex::encode(Sha256::digest(composition.to_string()));
            matter_invoices::insert_pending_operating(&plan, firm, &request.reason, &digest).await?
        }
        (None, None) => matter_invoices::insert_pending(&plan).await?,
    };
    if ledger.amount_cents != total {
        return Err(MatterInvoiceError::Conflict("request id").into());
    }
    let stripe_id = if let Some(id) = ledger.external_id.clone() {
        id
    } else {
        in_stripe(
            &stripe,
            firm,
            &account,
            &party,
            &ledger,
            &line_items,
            (input, memo),
        )
        .await?
    };
    let invoice = matter_invoices::attach_stripe(project_id, ledger.id, &stripe_id).await?;
    Ok(Draft {
        invoice,
        line_items,
        coupon: input.coupon.clone(),
    })
}

/// The Stripe client for an invoice already frozen to its destination.
async fn client_for<'a>(
    state: &ApiState,
    kms: &dyn cloud::RuntimeKms,
    key: &'a mut Option<store::firm_secrets::ResolvedCredential>,
    project_id: Uuid,
    invoice: &MatterInvoice,
) -> Result<billing::stripe::StripeInvoiceClient<'a>, InvoiceError> {
    let store = &state.application_store;
    let project = store::projects::find_by_id(store, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;
    let firm = project
        .firm_id
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let route = if invoice.destination.as_deref() == Some("operating") {
        None
    } else {
        Some(
            store::firm_secrets::stripe_trust_route(store, &project)
                .await
                .map_err(|_| InvoiceGenerationError::StripeUnavailable)?
                .ok_or(InvoiceError::DestinationChanged)?,
        )
    };
    let kind = route.as_ref().map_or(
        store::firm_secrets::IntegrationSecretKind::StripeOperatingAppKey,
        store::firm_secrets::TrustRoute::credential_kind,
    );
    *key = billing_api::stripe_credential(store, kms, firm, kind).await?;
    let key = key
        .as_ref()
        .ok_or(InvoiceGenerationError::StripeUnavailable)?;
    let stripe = billing_api::stripe_client(key, route.as_ref()).await?;
    if invoice.stripe_account_id.as_deref() != Some(stripe.account_id().await?.as_str()) {
        return Err(InvoiceError::DestinationChanged);
    }
    Ok(stripe)
}

async fn stripe_invoice(project: Uuid, id: Uuid) -> Result<(MatterInvoice, String), InvoiceError> {
    let invoice = matter_invoices::find(project, id)
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    let stripe_id = invoice
        .external_id
        .clone()
        .filter(|_| invoice.source == Source::Stripe)
        .ok_or(InvoiceError::Invalid(
            "only a Stripe invoice is sent from Navigator",
        ))?;
    Ok((invoice, stripe_id))
}

/// `POST .../invoices/{invoice}/send` — finalize the reviewed draft and send
/// it. A zero-balance invoice is finalized as a record and not sent.
pub(crate) async fn send(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Extension(kms): Extension<Arc<dyn cloud::RuntimeKms>>,
    Path((project, id)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, project, true).await?;
    let result = async {
        let (invoice, stripe_id) = stripe_invoice(project, id).await?;
        if invoice.status != Status::Draft {
            return Ok(invoice);
        }
        let mut key = None;
        let stripe = client_for(&state, kms.as_ref(), &mut key, project, &invoice).await?;
        let sent = stripe.finalize_and_send(invoice.id, &stripe_id).await?;
        let invoice = matter_invoices::set_status(project, id, Status::Sent, None).await?;
        if sent.total == 0 {
            return Ok(matter_invoices::set_status(project, id, Status::Paid, None).await?);
        }
        Ok(invoice)
    }
    .await;
    Ok(reply(result, false, "send_invoice", project))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CloseInput {
    reason: String,
}

/// `POST .../invoices/{invoice}/{void,uncollectible}` — close a sent
/// invoice with the reason, in Stripe first for a Stripe invoice.
pub(crate) async fn close(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Extension(kms): Extension<Arc<dyn cloud::RuntimeKms>>,
    Path((project, id, verb)): Path<(Uuid, Uuid, String)>,
    Json(input): Json<CloseInput>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, project, true).await?;
    let status = match verb.as_str() {
        "void" => Status::Void,
        "uncollectible" => Status::Uncollectible,
        _ => return Err(ApiError::NotFound),
    };
    let result = async {
        let invoice = matter_invoices::find(project, id)
            .await?
            .ok_or(MatterInvoiceError::NotFound)?;
        if input.reason.trim().is_empty() {
            return Err(InvoiceError::Invalid(
                "voiding or marking an invoice uncollectible needs a reason",
            ));
        }
        if invoice.source == Source::Stripe && invoice.status != status {
            if invoice.status != Status::Sent {
                return Err(InvoiceError::Invalid(
                    "only a sent Stripe invoice is voided or marked uncollectible",
                ));
            }
            let (_, stripe_id) = stripe_invoice(project, id).await?;
            let mut key = None;
            let stripe = client_for(&state, kms.as_ref(), &mut key, project, &invoice).await?;
            stripe
                .close_invoice(invoice.id, &stripe_id, status == Status::Uncollectible)
                .await?;
        }
        Ok(matter_invoices::set_status(project, id, status, Some(&input.reason)).await?)
    }
    .await;
    Ok(reply(result, false, "close_invoice", project))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PaymentInput {
    amount_cents: i64,
    received_on: NaiveDate,
    source: Source,
    #[serde(default)]
    external_id: Option<String>,
    #[serde(default)]
    payer: Option<String>,
    /// Why the payment is recorded here: a partial payment, a check, cash.
    reason: String,
}

/// `POST .../invoices/{invoice}/payments` — record a payment received
/// outside Stripe, in full or in part, with the reason.
pub(crate) async fn payment(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((project, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<PaymentInput>,
) -> Result<Response, ApiError> {
    let (actor, _) = authorize(&state, &session, project, true).await?;
    let result = async {
        if input.reason.trim().is_empty() || input.source == Source::Stripe {
            return Err(InvoiceError::Invalid(
                "a recorded payment names its reason and a source other than Stripe",
            ));
        }
        let (invoice, payment) = matter_invoices::record_payment(
            project,
            id,
            Some(actor),
            &matter_invoices::NewPayment {
                amount_cents: input.amount_cents,
                received_on: input.received_on,
                source: input.source,
                external_id: input.external_id.clone(),
                payer: input.payer.clone(),
                memo: Some(input.reason.clone()),
                client_visible: true,
            },
        )
        .await?;
        Ok(serde_json::json!({ "invoice": invoice, "payment": payment }))
    }
    .await;
    Ok(reply(result, true, "record_payment", project))
}

/// `POST .../invoices/records` — record an invoice paid or issued outside
/// Stripe: in person, by check, by wire, or otherwise.
pub(crate) async fn record(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project): Path<Uuid>,
    Json(input): Json<matter_invoices::NewRecord>,
) -> Result<Response, ApiError> {
    let (actor, _) = authorize(&state, &session, project, true).await?;
    let result = matter_invoices::record(project, Some(actor), &input)
        .await
        .map_err(InvoiceError::from);
    Ok(reply(result, true, "record_invoice", project))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(value: serde_json::Value) -> DraftInput {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn discounts_write_offs_and_credits_are_reasoned_lines() {
        let request = Uuid::nil();
        let (lines, total) = compose(&input(serde_json::json!({
            "request_id": request,
            "lines": [
                {"description": "Retainer", "unit_amount_cents": 150_000},
                {"description": "Consultation", "unit_amount_cents": 20_000,
                 "discount": {"percent_bps": 10_000, "reason": "Courtesy"}},
                {"description": "Filing prep", "quantity": 2, "unit_amount_cents": 5_000,
                 "discount": {"amount_cents": 1_000, "reason": "Returning client"}},
            ],
            "discount": {"percent_bps": 1_000, "reason": "Referral"},
            "credit": {"amount_cents": 50_000, "reason": "Deposit received 2026-11-01"},
        })))
        .unwrap();
        let described: Vec<_> = lines
            .iter()
            .map(|line| (line.description.as_str(), line.unit_amount_cents))
            .collect();
        assert_eq!(
            described,
            [
                ("Retainer", 150_000),
                ("Consultation", 20_000),
                ("Write-off: Consultation — Courtesy", -20_000),
                ("Filing prep (2 × $50.00)", 10_000),
                ("Discount: Filing prep — Returning client", -1_000),
                ("Discount — Referral", -15_900),
                (
                    "Credit for prior payment — Deposit received 2026-11-01",
                    -50_000
                ),
            ]
        );
        assert_eq!(total, 93_100);
    }

    #[test]
    fn a_zero_balance_is_allowed_and_a_negative_one_or_unreasoned_discount_is_not() {
        let base = |extra: serde_json::Value| {
            let mut value = serde_json::json!({
                "request_id": Uuid::nil(),
                "lines": [{"description": "Advice", "unit_amount_cents": 10_000}],
            });
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            compose(&input(value))
        };
        assert_eq!(
            base(
                serde_json::json!({"credit": {"amount_cents": 10_000, "reason": "Paid at consult"}})
            )
            .unwrap()
            .1,
            0
        );
        assert!(base(
            serde_json::json!({"credit": {"amount_cents": 10_001, "reason": "Too much"}})
        )
        .is_err());
        assert!(
            base(serde_json::json!({"discount": {"percent_bps": 500, "reason": " "}})).is_err()
        );
        assert!(
            base(serde_json::json!({"discount": {"percent_bps": 10_001, "reason": "x"}})).is_err()
        );
    }

    #[test]
    fn terms_set_the_due_date() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
        let with = |extra: serde_json::Value| {
            let mut value = serde_json::json!({"request_id": Uuid::nil(), "lines": []});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            due_on(&input(value), today)
        };
        assert_eq!(with(serde_json::json!({})).unwrap(), today);
        assert_eq!(
            with(serde_json::json!({"terms": "net_30"})).unwrap(),
            today + chrono::Days::new(30)
        );
        let day = NaiveDate::from_ymd_opt(2026, 12, 15).unwrap();
        assert_eq!(with(serde_json::json!({"due_on": day})).unwrap(), day);
        assert!(with(serde_json::json!({"due_on": "2026-11-01"})).is_err());
        assert!(with(serde_json::json!({"due_on": day, "terms": "net_15"})).is_err());
    }
}
