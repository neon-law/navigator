//! The Matter Invoice ledger (LAW-507): every invoice a matter is billed,
//! with its payments as separate rows.
//!
//! Stripe is the only invoice issuer. Money collected outside Stripe — in
//! person, by check or wire — is recorded by hand under its `source` and
//! `external_id`. Invoices Xero issued before it was retired are kept as
//! `source = 'xero'` history. The balance due is the invoice less its
//! payments; a credit on a Navigator-composed invoice names the payment it
//! applies. See `docs/glossary/matter-invoice.md`.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::accounting as authority;

/// How a payment reference names the Project it settles: `Matter <uuid or
/// code>`, the convention trust deposits and refunds carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectScopeRef {
    Id(Uuid),
    Code(String),
}

/// Parse `Matter <project uuid or code>`. `None` when the prefix is missing
/// or the remainder is neither a UUID nor a valid Project code.
#[must_use]
pub fn project_scope_from_reference(reference: &str) -> Option<ProjectScopeRef> {
    let token = reference.trim().strip_prefix("Matter ")?.trim();
    if token.is_empty() {
        return None;
    }
    if let Ok(id) = Uuid::parse_str(token) {
        return Some(ProjectScopeRef::Id(id));
    }
    crate::projects::is_valid_code(token).then(|| ProjectScopeRef::Code(token.to_string()))
}

/// Resolve [`project_scope_from_reference`] against the store.
///
/// # Errors
/// [`MatterInvoiceError::Projects`] when the lookup itself fails.
pub async fn resolve_project_scope(
    db: &ApplicationStore,
    reference: &str,
) -> Result<Option<Uuid>, MatterInvoiceError> {
    match project_scope_from_reference(reference) {
        Some(ProjectScopeRef::Id(id)) => Ok(crate::projects::find_by_id(db, id)
            .await?
            .map(|project| project.id)),
        Some(ProjectScopeRef::Code(code)) => Ok(crate::projects::find_by_code(db, &code)
            .await?
            .map(|project| project.id)),
        None => Ok(None),
    }
}

/// Where an invoice came from, or where money arrived outside Stripe.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Stripe,
    InPerson,
    Check,
    Wire,
    Other,
    /// History: an invoice Xero issued before it was retired.
    Xero,
}

impl Source {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stripe => "stripe",
            Self::InPerson => "in_person",
            Self::Check => "check",
            Self::Wire => "wire",
            Self::Other => "other",
            Self::Xero => "xero",
        }
    }
}

/// An invoice's state. `pending` is a Navigator-composed Stripe invoice
/// whose draft is not created yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Draft,
    Sent,
    Paid,
    Void,
    Uncollectible,
}

impl Status {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::Paid => "paid",
            Self::Void => "void",
            Self::Uncollectible => "uncollectible",
        }
    }

    /// Whether the invoice still asks to be paid.
    #[must_use]
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Sent)
    }
}

/// One ledger invoice with the payments recorded against it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterInvoice {
    pub id: Uuid,
    pub project_id: Uuid,
    pub source: Source,
    pub external_id: Option<String>,
    pub reference: String,
    pub status: Status,
    pub amount_cents: i64,
    /// The sum of the invoice's payments.
    pub amount_paid_cents: i64,
    pub currency: String,
    pub issued_on: Option<NaiveDate>,
    pub due_on: Option<NaiveDate>,
    pub payer: Option<String>,
    pub memo: Option<String>,
    pub status_reason: Option<String>,
    pub client_visible: bool,
    pub stripe_account_id: Option<String>,
    pub destination: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl MatterInvoice {
    /// What the invoice still asks to be paid; zero once it is settled,
    /// void, or uncollectible.
    #[must_use]
    pub fn balance_due_cents(&self) -> i64 {
        if matches!(self.status, Status::Sent | Status::Draft) {
            (self.amount_cents - self.amount_paid_cents).max(0)
        } else {
            0
        }
    }
}

/// One payment received against an invoice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Payment {
    pub id: Uuid,
    pub invoice_id: Uuid,
    pub amount_cents: i64,
    pub received_on: NaiveDate,
    pub source: Source,
    pub external_id: Option<String>,
    pub payer: Option<String>,
    pub memo: Option<String>,
    pub client_visible: bool,
    pub inserted_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum MatterInvoiceError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("no such invoice on this Project")]
    NotFound,
    #[error("that {0} is already recorded with different details")]
    Conflict(&'static str),
    #[error(transparent)]
    Projects(#[from] crate::projects::ProjectStoreError),
    #[error(transparent)]
    Brands(#[from] crate::brands::BrandError),
    #[error(transparent)]
    Postgres(#[from] authority::AccountingAuthorityError),
}

/// Every invoice column plus its paid-to-date, as one JSON row.
macro_rules! invoice_row {
    ($alias:literal) => {
        concat!(
            "to_jsonb(", $alias, ") || jsonb_build_object('amount_paid_cents', ",
            "coalesce((SELECT sum(p.amount_cents) FROM matter_invoice_payment p WHERE p.invoice_id = ",
            $alias, ".id), 0)::bigint) AS row"
        )
    };
}

/// Every invoice for a set of matters, newest first.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] if the lookup fails.
pub async fn for_projects(
    _db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<MatterInvoice>, MatterInvoiceError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        invoice_row!("m"),
        " FROM matter_invoice m WHERE m.project_id = ANY($1) ",
        "ORDER BY m.issued_on DESC NULLS FIRST, m.inserted_at DESC"
    ))
    .bind(project_ids.to_vec());
    Ok(authority::rows(&pg, query).await?)
}

/// One invoice on `project`.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] if the lookup fails.
pub async fn find(project: Uuid, id: Uuid) -> Result<Option<MatterInvoice>, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        invoice_row!("m"),
        " FROM matter_invoice m WHERE m.project_id = $1 AND m.id = $2"
    ))
    .bind(project)
    .bind(id);
    Ok(authority::maybe(&pg, query).await?)
}

/// The payments recorded against one invoice, oldest first.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] if the lookup fails.
pub async fn payments(invoice: Uuid) -> Result<Vec<Payment>, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(p) AS row FROM matter_invoice_payment p \
         WHERE p.invoice_id = $1 ORDER BY p.received_on, p.inserted_at, p.id",
    )
    .bind(invoice);
    Ok(authority::rows(&pg, query).await?)
}

/// Resolve the invoice a trust withdrawal line settles: by its issuer id (a
/// Stripe invoice id, a Xero id kept as history), by its ledger id, or by
/// its reference when exactly one invoice carries it. An ambiguous line is
/// `None`, so the caller refuses the whole withdrawal.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] when a lookup fails.
pub async fn find_for_allocation(
    _db: &ApplicationStore,
    reference: &str,
) -> Result<Option<MatterInvoice>, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let by_id = Uuid::parse_str(reference.trim()).ok();
    for query in [
        sqlx::query(concat!(
            "SELECT ",
            invoice_row!("m"),
            " FROM matter_invoice m WHERE m.external_id = $1 OR m.id = $2 LIMIT 2"
        ))
        .bind(reference.trim())
        .bind(by_id),
        sqlx::query(concat!(
            "SELECT ",
            invoice_row!("m"),
            " FROM matter_invoice m WHERE m.reference = $1 LIMIT 2"
        ))
        .bind(reference.trim())
        .bind(by_id),
    ] {
        let mut matches: Vec<MatterInvoice> = authority::rows(&pg, query).await?;
        match matches.len() {
            1 => return Ok(Some(matches.remove(0))),
            0 => {}
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// An invoice recorded by hand: money collected outside Stripe, or a Stripe
/// invoice issued outside Navigator.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRecord {
    pub source: Source,
    pub external_id: String,
    pub amount_cents: i64,
    #[serde(default = "usd")]
    pub currency: String,
    pub status: Status,
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub issued_on: Option<NaiveDate>,
    #[serde(default)]
    pub due_on: Option<NaiveDate>,
    #[serde(default)]
    pub payer: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
    #[serde(default)]
    pub client_visible: bool,
    #[serde(default)]
    pub receipt_document_id: Option<Uuid>,
}

fn usd() -> String {
    "USD".into()
}

fn text(value: Option<&str>, limit: usize) -> bool {
    value.is_none_or(|value| {
        !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
    })
}

impl NewRecord {
    fn validate(&self) -> Result<(), MatterInvoiceError> {
        if matches!(self.source, Source::Xero) {
            return Err(MatterInvoiceError::Invalid(
                "Xero no longer issues invoices; record the payment's own source",
            ));
        }
        if matches!(self.status, Status::Pending) {
            return Err(MatterInvoiceError::Invalid(
                "a recorded invoice is draft, sent, paid, void, or uncollectible",
            ));
        }
        if self.amount_cents < 0 {
            return Err(MatterInvoiceError::Invalid(
                "amount must be zero or more cents",
            ));
        }
        if !(self.currency.len() == 3 && self.currency.bytes().all(|b| b.is_ascii_uppercase())) {
            return Err(MatterInvoiceError::Invalid(
                "currency must be an ISO code like USD",
            ));
        }
        if !text(Some(&self.external_id), 200)
            || !text(self.reference.as_deref(), 200)
            || !text(self.payer.as_deref(), 300)
            || !text(self.memo.as_deref(), 2000)
        {
            return Err(MatterInvoiceError::Invalid(
                "check the external id, reference, payer, and memo",
            ));
        }
        if self.source == Source::Stripe && !self.external_id.starts_with("in_") {
            return Err(MatterInvoiceError::Invalid(
                "a Stripe invoice's external id is its in_ id",
            ));
        }
        Ok(())
    }
}

/// Record one invoice by hand. Recording the same source and external id
/// again with the same amount returns the first row; a different amount is
/// a conflict. A Stripe invoice Navigator already holds is matched by its
/// id, never duplicated.
///
/// # Errors
/// [`MatterInvoiceError::Invalid`] for a bad record;
/// [`MatterInvoiceError::Conflict`] for a reused id with other details.
pub async fn record(
    project: Uuid,
    actor: Option<Uuid>,
    input: &NewRecord,
) -> Result<MatterInvoice, MatterInvoiceError> {
    input.validate()?;
    let pg = authority::authority().await?;
    let reference = input.reference.clone().unwrap_or_else(|| {
        format!(
            "{} {}",
            match input.source {
                Source::Stripe => "Stripe invoice",
                Source::InPerson => "Collected in person",
                Source::Check => "Check",
                Source::Wire => "Wire",
                Source::Other | Source::Xero => "Payment",
            },
            input.external_id.trim()
        )
    });
    let query = sqlx::query(
        "INSERT INTO matter_invoice (id,project_id,source,external_id,reference,status,amount_cents,
         currency,issued_on,due_on,payer,memo,client_visible,receipt_document_id,created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
         ON CONFLICT (project_id,source,external_id) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(project)
    .bind(input.source.as_str())
    .bind(input.external_id.trim())
    .bind(reference.trim())
    .bind(input.status.as_str())
    .bind(input.amount_cents)
    .bind(&input.currency)
    .bind(input.issued_on)
    .bind(input.due_on)
    .bind(input.payer.as_deref().map(str::trim))
    .bind(input.memo.as_deref().map(str::trim))
    .bind(input.client_visible)
    .bind(input.receipt_document_id)
    .bind(actor);
    authority::execute(&pg, query).await?;
    let existing = by_external(project, input.source, input.external_id.trim())
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    if existing.amount_cents != input.amount_cents || existing.currency != input.currency {
        return Err(MatterInvoiceError::Conflict("invoice"));
    }
    Ok(existing)
}

/// The invoice one issuer id names on `project`.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] if the lookup fails.
pub async fn by_external(
    project: Uuid,
    source: Source,
    external_id: &str,
) -> Result<Option<MatterInvoice>, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        invoice_row!("m"),
        " FROM matter_invoice m WHERE m.project_id = $1 AND m.source = $2 AND m.external_id = $3"
    ))
    .bind(project)
    .bind(source.as_str())
    .bind(external_id);
    Ok(authority::maybe(&pg, query).await?)
}

/// A payment received against an invoice.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPayment {
    pub amount_cents: i64,
    pub received_on: NaiveDate,
    pub source: Source,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub payer: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
    #[serde(default)]
    pub client_visible: bool,
}

/// Record a payment. A payment that settles the balance marks a sent
/// invoice paid. Recording the same source and external id again returns
/// the first payment.
///
/// # Errors
/// [`MatterInvoiceError::Invalid`] for a bad payment or a void invoice;
/// [`MatterInvoiceError::NotFound`] for an invoice off the Project.
pub async fn record_payment(
    project: Uuid,
    invoice: Uuid,
    actor: Option<Uuid>,
    input: &NewPayment,
) -> Result<(MatterInvoice, Payment), MatterInvoiceError> {
    if input.amount_cents <= 0
        || input.received_on > Utc::now().date_naive()
        || matches!(input.source, Source::Xero)
        || !text(input.external_id.as_deref(), 200)
        || !text(input.payer.as_deref(), 300)
        || !text(input.memo.as_deref(), 2000)
    {
        return Err(MatterInvoiceError::Invalid(
            "a payment is a positive amount received today or earlier, from its own source",
        ));
    }
    let current = find(project, invoice)
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    if matches!(current.status, Status::Void | Status::Pending) {
        return Err(MatterInvoiceError::Invalid(
            "a void or uncreated invoice takes no payment",
        ));
    }
    let pg = authority::authority().await?;
    let id = Uuid::now_v7();
    let query = sqlx::query(
        "INSERT INTO matter_invoice_payment (id,invoice_id,amount_cents,received_on,source,external_id,
         payer,memo,client_visible,recorded_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
         ON CONFLICT (invoice_id,source,external_id) DO NOTHING",
    )
    .bind(id)
    .bind(invoice)
    .bind(input.amount_cents)
    .bind(input.received_on)
    .bind(input.source.as_str())
    .bind(input.external_id.as_deref().map(str::trim))
    .bind(input.payer.as_deref().map(str::trim))
    .bind(input.memo.as_deref().map(str::trim))
    .bind(input.client_visible)
    .bind(actor);
    authority::execute(&pg, query).await?;
    let recorded = payments(invoice)
        .await?
        .into_iter()
        .find(|payment| {
            payment.id == id
                || (input.external_id.is_some()
                    && payment.source == input.source
                    && payment.external_id.as_deref()
                        == input.external_id.as_deref().map(str::trim))
        })
        .ok_or(MatterInvoiceError::NotFound)?;
    if recorded.amount_cents != input.amount_cents {
        return Err(MatterInvoiceError::Conflict("payment"));
    }
    let settle = sqlx::query(
        "UPDATE matter_invoice m SET status='paid', updated_at=now() WHERE m.id=$1 AND m.status='sent'
         AND (SELECT coalesce(sum(p.amount_cents),0) FROM matter_invoice_payment p WHERE p.invoice_id=m.id) >= m.amount_cents",
    )
    .bind(invoice);
    authority::execute(&pg, settle).await?;
    let invoice = find(project, invoice)
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    Ok((invoice, recorded))
}

/// Move an invoice to `status` with a reason: sent, void, uncollectible, or
/// paid. A void or uncollectible invoice needs the reason. A sent invoice
/// has reached the client, so the client's portal shows it from then on.
///
/// # Errors
/// [`MatterInvoiceError::Invalid`] for a missing reason or a transition the
/// ledger does not take.
pub async fn set_status(
    project: Uuid,
    invoice: Uuid,
    status: Status,
    reason: Option<&str>,
) -> Result<MatterInvoice, MatterInvoiceError> {
    let current = find(project, invoice)
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    let needs_reason = matches!(status, Status::Void | Status::Uncollectible);
    if (needs_reason && reason.is_none_or(|reason| reason.trim().is_empty())) || !text(reason, 1000)
    {
        return Err(MatterInvoiceError::Invalid(
            "voiding or marking an invoice uncollectible needs a reason",
        ));
    }
    let allowed = match (current.status, status) {
        (from, to) if from == to => true,
        (Status::Draft, Status::Sent | Status::Void)
        | (Status::Sent, Status::Paid | Status::Void | Status::Uncollectible)
        | (Status::Uncollectible, Status::Paid) => true,
        _ => false,
    };
    if !allowed {
        return Err(MatterInvoiceError::Invalid(
            "the invoice cannot move to that status from where it is",
        ));
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE matter_invoice SET status=$3, status_reason=coalesce($4,status_reason),
         client_visible = client_visible OR $3 = 'sent', updated_at=now()
         WHERE project_id=$1 AND id=$2",
    )
    .bind(project)
    .bind(invoice)
    .bind(status.as_str())
    .bind(reason.map(str::trim));
    authority::execute(&pg, query).await?;
    find(project, invoice)
        .await?
        .ok_or(MatterInvoiceError::NotFound)
}

/// A Navigator-composed Stripe invoice, frozen before the provider call so a
/// retry presents exactly what the lawyer reviewed.
#[derive(Clone, Debug)]
pub struct DraftPlan {
    pub project_id: Uuid,
    pub request_id: Uuid,
    pub reference: String,
    pub amount_cents: i64,
    pub currency: String,
    pub issued_on: NaiveDate,
    pub due_on: NaiveDate,
    pub memo: Option<String>,
    /// The lines, discounts, credits, and terms the draft carries.
    pub composition: serde_json::Value,
    /// `trust` or `operating`.
    pub destination: &'static str,
    pub trust_state: Option<String>,
    pub stripe_account_id: String,
    pub destination_approval_id: Option<Uuid>,
    pub billed_person_id: Uuid,
    pub created_by: Uuid,
}

/// Freeze a Stripe invoice as `pending`. Replaying the same request id on
/// the Project returns the first row.
///
/// # Errors
/// [`MatterInvoiceError::Conflict`] when the request id names other terms.
pub async fn insert_pending(plan: &DraftPlan) -> Result<MatterInvoice, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "INSERT INTO matter_invoice (id,project_id,source,reference,status,amount_cents,currency,
         issued_on,due_on,memo,request_id,composition,destination,trust_state,stripe_account_id,
         destination_approval_id,billed_person_id,created_by)
         VALUES ($1,$2,'stripe',$3,'pending',$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
         ON CONFLICT (project_id,request_id) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(plan.project_id)
    .bind(plan.reference.trim())
    .bind(plan.amount_cents)
    .bind(&plan.currency)
    .bind(plan.issued_on)
    .bind(plan.due_on)
    .bind(plan.memo.as_deref().map(str::trim))
    .bind(plan.request_id)
    .bind(&plan.composition)
    .bind(plan.destination)
    .bind(plan.trust_state.as_deref())
    .bind(&plan.stripe_account_id)
    .bind(plan.destination_approval_id)
    .bind(plan.billed_person_id)
    .bind(plan.created_by);
    authority::execute(&pg, query).await?;
    let existing = by_request(plan.project_id, plan.request_id)
        .await?
        .ok_or(MatterInvoiceError::NotFound)?;
    replayed(existing, plan)
}

/// Freeze an Operating invoice with the lawyer's approval in one
/// transaction (ENG-1343): the approval names who, when, why, and the
/// reviewed `digest`; the invoice references it. A replayed request id
/// returns the first row and records no second approval.
///
/// # Errors
/// [`MatterInvoiceError::Conflict`] when the request id names other terms.
pub async fn insert_pending_operating(
    plan: &DraftPlan,
    firm: Uuid,
    reason: &str,
    digest: &str,
) -> Result<MatterInvoice, MatterInvoiceError> {
    if let Some(existing) = by_request(plan.project_id, plan.request_id).await? {
        return replayed(existing, plan);
    }
    let pg = authority::authority().await?;
    let mut tx = pg
        .pool()
        .begin()
        .await
        .map_err(authority::AccountingAuthorityError::from)?;
    let approval = crate::invoice_destinations::record_in_tx(
        &mut tx,
        firm,
        plan.project_id,
        plan.created_by,
        reason,
        digest,
    )
    .await
    .map_err(|_| MatterInvoiceError::Invalid("the Operating approval could not be recorded"))?;
    sqlx::query(
        "INSERT INTO matter_invoice (id,project_id,source,reference,status,amount_cents,currency,
         issued_on,due_on,memo,request_id,composition,destination,stripe_account_id,
         destination_approval_id,billed_person_id,created_by)
         VALUES ($1,$2,'stripe',$3,'pending',$4,$5,$6,$7,$8,$9,$10,'operating',$11,$12,$13,$14)",
    )
    .bind(Uuid::now_v7())
    .bind(plan.project_id)
    .bind(plan.reference.trim())
    .bind(plan.amount_cents)
    .bind(&plan.currency)
    .bind(plan.issued_on)
    .bind(plan.due_on)
    .bind(plan.memo.as_deref().map(str::trim))
    .bind(plan.request_id)
    .bind(&plan.composition)
    .bind(&plan.stripe_account_id)
    .bind(approval.id)
    .bind(plan.billed_person_id)
    .bind(plan.created_by)
    .execute(&mut *tx)
    .await
    .map_err(authority::AccountingAuthorityError::from)?;
    tx.commit()
        .await
        .map_err(authority::AccountingAuthorityError::from)?;
    by_request(plan.project_id, plan.request_id)
        .await?
        .ok_or(MatterInvoiceError::NotFound)
}

fn replayed(
    existing: MatterInvoice,
    plan: &DraftPlan,
) -> Result<MatterInvoice, MatterInvoiceError> {
    if existing.amount_cents != plan.amount_cents
        || existing.stripe_account_id.as_deref() != Some(plan.stripe_account_id.as_str())
    {
        return Err(MatterInvoiceError::Conflict("request id"));
    }
    Ok(existing)
}

/// The invoice one request id froze on `project`.
///
/// # Errors
/// [`MatterInvoiceError::Postgres`] if the lookup fails.
pub async fn by_request(
    project: Uuid,
    request: Uuid,
) -> Result<Option<MatterInvoice>, MatterInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(
        "SELECT ",
        invoice_row!("m"),
        " FROM matter_invoice m WHERE m.project_id = $1 AND m.request_id = $2"
    ))
    .bind(project)
    .bind(request);
    Ok(authority::maybe(&pg, query).await?)
}

/// The frozen composition and first attempt of a pending invoice.
///
/// # Errors
/// [`MatterInvoiceError::NotFound`] off the Project.
pub async fn composition(
    project: Uuid,
    id: Uuid,
) -> Result<(serde_json::Value, Option<DateTime<Utc>>), MatterInvoiceError> {
    let pg = authority::authority().await?;
    let row: Option<(Option<serde_json::Value>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "UPDATE matter_invoice SET first_attempted_at=coalesce(first_attempted_at, now())
         WHERE project_id=$1 AND id=$2 RETURNING composition, first_attempted_at",
    )
    .bind(project)
    .bind(id)
    .fetch_optional(pg.pool())
    .await
    .map_err(authority::AccountingAuthorityError::from)?;
    let (composition, attempted) = row.ok_or(MatterInvoiceError::NotFound)?;
    Ok((composition.unwrap_or(serde_json::Value::Null), attempted))
}

/// Bind the created Stripe draft to its pending row. Binding the same id
/// again changes nothing.
///
/// # Errors
/// [`MatterInvoiceError::Conflict`] when the row already names another
/// Stripe invoice.
pub async fn attach_stripe(
    project: Uuid,
    id: Uuid,
    stripe_invoice_id: &str,
) -> Result<MatterInvoice, MatterInvoiceError> {
    if !stripe_invoice_id.starts_with("in_") {
        return Err(MatterInvoiceError::Invalid(
            "a Stripe invoice id starts with in_",
        ));
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE matter_invoice SET external_id=$3,
         status=CASE WHEN status='pending' THEN 'draft' ELSE status END, updated_at=now()
         WHERE project_id=$1 AND id=$2 AND source='stripe' AND (external_id IS NULL OR external_id=$3)",
    )
    .bind(project)
    .bind(id)
    .bind(stripe_invoice_id);
    if authority::execute(&pg, query).await? != 1 {
        return Err(MatterInvoiceError::Conflict("Stripe invoice"));
    }
    find(project, id).await?.ok_or(MatterInvoiceError::NotFound)
}

/// One invoice as the Firm-wide invoice graphs consume it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FirmInvoice {
    pub project_id: Uuid,
    pub invoice_id: Uuid,
    pub status: Status,
    pub amount_cents: i64,
    pub amount_paid_cents: i64,
    pub issued_on: NaiveDate,
    pub currency: String,
    pub brand: String,
    pub lawyer_dris: Vec<String>,
}

/// Every issued (not draft, pending, or void) invoice on or after `since`
/// for every Project a Firm owns.
///
/// # Errors
/// [`MatterInvoiceError`] if a read fails.
pub async fn for_firm_since(
    db: &ApplicationStore,
    firm_id: Uuid,
    since: NaiveDate,
) -> Result<Vec<FirmInvoice>, MatterInvoiceError> {
    let brand_by_project: std::collections::HashMap<Uuid, String> = crate::projects::all(db)
        .await?
        .into_iter()
        .filter(|project| project.firm_id == Some(firm_id))
        .map(|project| (project.id, project.brand))
        .collect();
    if brand_by_project.is_empty() {
        return Ok(Vec::new());
    }
    let project_ids: Vec<Uuid> = brand_by_project.keys().copied().collect();
    let lawyer_dris =
        crate::projects::dri_names_by_project(db, crate::projects::DriSide::Lawyer).await?;
    Ok(for_projects(db, &project_ids)
        .await?
        .into_iter()
        .filter(|invoice| {
            matches!(
                invoice.status,
                Status::Sent | Status::Paid | Status::Uncollectible
            )
        })
        .filter_map(|invoice| {
            let issued_on = invoice.issued_on.filter(|issued| *issued >= since)?;
            Some(FirmInvoice {
                lawyer_dris: lawyer_dris
                    .get(&invoice.project_id)
                    .cloned()
                    .unwrap_or_default(),
                brand: brand_by_project.get(&invoice.project_id)?.clone(),
                project_id: invoice.project_id,
                invoice_id: invoice.id,
                status: invoice.status,
                amount_cents: invoice.amount_cents,
                amount_paid_cents: invoice.amount_paid_cents,
                issued_on,
                currency: invoice.currency,
            })
        })
        .collect())
}

/// One label's summed cents within a [`CurrencyInvoiceRollup`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InvoiceGroupTotal {
    pub label: String,
    pub invoiced_cents: i64,
    pub paid_cents: i64,
}

/// The trailing-window rollup for one currency, never summed across
/// currencies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CurrencyInvoiceRollup {
    pub currency: String,
    pub total_invoiced_cents: i64,
    pub total_paid_cents: i64,
    pub by_brand: Vec<InvoiceGroupTotal>,
    pub by_lawyer_dri: Vec<InvoiceGroupTotal>,
}

/// The Firm show page's trailing-30-day invoice rollup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
pub struct FirmInvoiceRollup {
    pub currencies: Vec<CurrencyInvoiceRollup>,
}

const UNASSIGNED_DRI: &str = "Unassigned";

type GroupTotals = std::collections::BTreeMap<String, (i64, i64)>;

fn accumulate(totals: &mut GroupTotals, label: &str, invoiced_cents: i64, paid_cents: i64) {
    let entry = totals.entry(label.to_string()).or_insert((0, 0));
    entry.0 = entry.0.saturating_add(invoiced_cents);
    entry.1 = entry.1.saturating_add(paid_cents);
}

fn totals_into_groups(totals: GroupTotals) -> Vec<InvoiceGroupTotal> {
    totals
        .into_iter()
        .map(|(label, (invoiced_cents, paid_cents))| InvoiceGroupTotal {
            label,
            invoiced_cents,
            paid_cents,
        })
        .collect()
}

/// Every invoice issued in the last 30 days on one of the Firm's Projects,
/// grouped by brand and by lawyer DRI, per currency.
///
/// # Errors
/// [`MatterInvoiceError`] if a read fails.
pub async fn firm_thirty_day_rollup(
    db: &ApplicationStore,
    firm_id: Uuid,
    now: DateTime<Utc>,
) -> Result<FirmInvoiceRollup, MatterInvoiceError> {
    let since = (now - chrono::Duration::days(30)).date_naive();
    let invoices = for_firm_since(db, firm_id, since).await?;
    if invoices.is_empty() {
        return Ok(FirmInvoiceRollup::default());
    }
    let mut brand_name_by_key = std::collections::HashMap::new();
    for key in invoices
        .iter()
        .map(|invoice| invoice.brand.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(brand) = crate::brands::find_by_key(db, key).await? {
            brand_name_by_key.insert(key.to_string(), brand.name);
        }
    }
    let mut by_currency: std::collections::BTreeMap<String, (GroupTotals, GroupTotals)> =
        std::collections::BTreeMap::new();
    for invoice in &invoices {
        let (by_brand, by_lawyer_dri) = by_currency.entry(invoice.currency.clone()).or_default();
        let brand_label = brand_name_by_key
            .get(&invoice.brand)
            .cloned()
            .unwrap_or_else(|| invoice.brand.clone());
        accumulate(
            by_brand,
            &brand_label,
            invoice.amount_cents,
            invoice.amount_paid_cents,
        );
        if invoice.lawyer_dris.is_empty() {
            accumulate(
                by_lawyer_dri,
                UNASSIGNED_DRI,
                invoice.amount_cents,
                invoice.amount_paid_cents,
            );
        }
        for name in &invoice.lawyer_dris {
            accumulate(
                by_lawyer_dri,
                name,
                invoice.amount_cents,
                invoice.amount_paid_cents,
            );
        }
    }
    Ok(FirmInvoiceRollup {
        currencies: by_currency
            .into_iter()
            .map(
                |(currency, (by_brand, by_lawyer_dri))| CurrencyInvoiceRollup {
                    total_invoiced_cents: by_brand.values().map(|(i, _)| *i).sum(),
                    total_paid_cents: by_brand.values().map(|(_, p)| *p).sum(),
                    currency,
                    by_brand: totals_into_groups(by_brand),
                    by_lawyer_dri: totals_into_groups(by_lawyer_dri),
                },
            )
            .collect(),
    })
}

#[cfg(test)]
#[path = "matter_invoices/tests.rs"]
mod tests;
