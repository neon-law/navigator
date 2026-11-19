//! A Client's Purchase of an Offering.
//!
//! The catalog's name, price, and cadence are copied when a Purchase is
//! reserved, before any provider write. Xero invoices consultations; Stripe
//! hosts recurring Checkout and retains the accepted terms and provider
//! account binding. Only verified payment activates access. Stripe access
//! also ends at the last confirmed paid-through boundary. Stable Purchase
//! keys make safe provider retries idempotent. Complimentary consultations
//! never call a payment provider.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// How often an Offering is billed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Once,
    Month,
    Year,
}

impl Cadence {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "month" => Self::Month,
            "year" => Self::Year,
            _ => Self::Once,
        }
    }
}

/// Where a Purchase stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PurchaseState {
    /// Waived: a consultation for a Client who signed in with Google.
    Complimentary,
    /// Reserved before a provider invoice or checkout is created.
    Invoicing,
    /// An invoice or hosted Checkout awaits payment.
    Invoiced,
    /// Payment is verified; Stripe access also requires an unexpired paid period.
    Active,
    /// Hosted Checkout expired without creating a paid subscription.
    Expired,
}

impl PurchaseState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complimentary => "complimentary",
            Self::Invoicing => "invoicing",
            Self::Invoiced => "invoiced",
            Self::Active => "active",
            Self::Expired => "expired",
        }
    }

    fn parse(value: &str) -> Result<Self, PurchaseError> {
        Ok(match value {
            "complimentary" => Self::Complimentary,
            "invoicing" => Self::Invoicing,
            "invoiced" => Self::Invoiced,
            "active" => Self::Active,
            "expired" => Self::Expired,
            _ => return Err(PurchaseError::Store("unrecognized purchase state".into())),
        })
    }
}

/// One Purchase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Purchase {
    pub id: Uuid,
    pub person_id: Uuid,
    pub email: String,
    pub offering: String,
    pub offering_name: String,
    pub cadence: Cadence,
    pub amount_cents: i64,
    pub currency: String,
    /// The Xero chart-of-accounts code the revenue posts to.
    pub account_code: String,
    pub state: PurchaseState,
    pub xero_contact_id: Option<String>,
    pub xero_invoice_id: Option<String>,
    pub invoice_url: Option<String>,
    pub xero_repeating_invoice_id: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub activated_at: Option<DateTime<Utc>>,
    pub stripe: Option<StripePurchase>,
    pub renewal_cancelled_at: Option<DateTime<Utc>>,
    pub access_ends_at: Option<DateTime<Utc>>,
}

/// Provider identities and the verified paid-through boundary of a Stripe Purchase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StripePurchase {
    pub firm_id: Uuid,
    pub account_id: String,
    pub checkout_session_id: Option<String>,
    pub customer_id: Option<String>,
    pub subscription_id: Option<String>,
    pub period_end: Option<DateTime<Utc>>,
    pub cancel_at_period_end: bool,
    pub renewal_terms: String,
    pub consented_at: DateTime<Utc>,
}

impl Purchase {
    /// Stripe access ends at the last provider-confirmed paid-through boundary.
    #[must_use]
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.state == PurchaseState::Active
            && self.access_ends_at.is_none_or(|end| end > now)
            && self
                .stripe
                .as_ref()
                .is_none_or(|stripe| stripe.period_end.is_some_and(|end| end > now))
    }
}

/// What a Client is buying, copied from the catalog at the moment of purchase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPurchase {
    pub person_id: Uuid,
    pub email: String,
    pub offering: String,
    pub offering_name: String,
    pub cadence: Cadence,
    pub amount_cents: i64,
    pub currency: String,
    pub account_code: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PurchaseError {
    #[error("purchase not found")]
    NotFound,
    #[error("a paid Purchase needs a positive amount")]
    InvalidAmount,
    #[error("purchase store error: {0}")]
    Store(String),
}

impl From<sqlx::Error> for PurchaseError {
    fn from(error: sqlx::Error) -> Self {
        Self::Store(error.to_string())
    }
}

const COLUMNS: &str = "id, person_id, email, offering, offering_name, cadence, amount_cents, \
     currency, account_code, state, xero_contact_id, xero_invoice_id, invoice_url, xero_repeating_invoice_id, \
     inserted_at, updated_at, activated_at, stripe_firm_id, stripe_account_id, stripe_checkout_session_id, \
     stripe_customer_id, stripe_subscription_id, stripe_period_end, stripe_cancel_at_period_end, \
     renewal_terms, renewal_consented_at, renewal_cancelled_at, access_ends_at";

/// The offerings whose Purchase is a Same Day Legal subscription: the one
/// subscription sold under two names, DeleteYourData.com and Same Day Legal. The
/// canonical catalog anchor — the portal's subscription gate and the monthly
/// sandbox credit reset both key on these ids, so they live next to the
/// Purchase model rather than in either caller.
pub const SAME_DAY_LEGAL_OFFERINGS: [&str; 2] =
    ["delete-your-data-monthly", "delete-your-data-annual"];

fn from_row(row: &sqlx::postgres::PgRow) -> Result<Purchase, PurchaseError> {
    Ok(Purchase {
        id: row.try_get("id")?,
        person_id: row.try_get("person_id")?,
        email: row.try_get("email")?,
        offering: row.try_get("offering")?,
        offering_name: row.try_get("offering_name")?,
        cadence: Cadence::parse(row.try_get("cadence")?),
        amount_cents: row.try_get("amount_cents")?,
        currency: row.try_get("currency")?,
        account_code: row.try_get("account_code")?,
        state: PurchaseState::parse(row.try_get("state")?)?,
        xero_contact_id: row.try_get("xero_contact_id")?,
        xero_invoice_id: row.try_get("xero_invoice_id")?,
        invoice_url: row.try_get("invoice_url")?,
        xero_repeating_invoice_id: row.try_get("xero_repeating_invoice_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
        activated_at: row.try_get("activated_at")?,
        renewal_cancelled_at: row.try_get("renewal_cancelled_at")?,
        access_ends_at: row.try_get("access_ends_at")?,
        stripe: match row.try_get::<Option<String>, _>("stripe_account_id")? {
            None => None,
            Some(account_id) => Some(StripePurchase {
                firm_id: row.try_get("stripe_firm_id")?,
                account_id,
                checkout_session_id: row.try_get("stripe_checkout_session_id")?,
                customer_id: row.try_get("stripe_customer_id")?,
                subscription_id: row.try_get("stripe_subscription_id")?,
                period_end: row.try_get("stripe_period_end")?,
                cancel_at_period_end: row.try_get("stripe_cancel_at_period_end")?,
                renewal_terms: row.try_get("renewal_terms")?,
                consented_at: row.try_get("renewal_consented_at")?,
            }),
        },
    })
}

/// The Client's open (`invoicing` or `invoiced`) Purchase of `new.offering`,
/// recording a new `invoicing` one when there is none.
///
/// Concurrent calls for the same Client and Offering converge on one row:
/// the partial unique index admits one open Purchase, and the loser of the
/// race reads the winner's.
///
/// # Errors
/// [`PurchaseError::InvalidAmount`] for a zero or negative price, or a
/// database error.
pub async fn open(
    pg: &PostgresDb,
    new: &NewPurchase,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    open_inner(pg, new, now, None).await
}

/// Accepted, server-owned recurring terms bound to one Firm's Stripe account.
pub struct StripeAdmission<'a> {
    pub firm_id: Uuid,
    pub account_id: &'a str,
    pub terms: &'a str,
}

/// Reserve a Stripe Purchase before the provider call; retries retain the original consent snapshot.
pub async fn open_stripe(
    pg: &PostgresDb,
    new: &NewPurchase,
    now: DateTime<Utc>,
    admission: StripeAdmission<'_>,
) -> Result<Purchase, PurchaseError> {
    if new.cadence == Cadence::Once || admission.terms.trim().is_empty() {
        return Err(PurchaseError::InvalidAmount);
    }
    let firm_id = admission.firm_id;
    let account_id = admission.account_id.to_owned();
    let terms = admission.terms.to_owned();
    let row = open_inner(pg, new, now, Some(admission)).await?;
    if row.amount_cents != new.amount_cents
        || row.cadence != new.cadence
        || row.offering_name != new.offering_name
        || row.stripe.as_ref().is_none_or(|stripe| {
            stripe.firm_id != firm_id
                || stripe.account_id != account_id
                || stripe.renewal_terms != terms
        })
    {
        return Err(PurchaseError::Store(
            "an existing purchase requires reconciliation".into(),
        ));
    }
    Ok(row)
}

async fn open_inner(
    pg: &PostgresDb,
    new: &NewPurchase,
    now: DateTime<Utc>,
    stripe: Option<StripeAdmission<'_>>,
) -> Result<Purchase, PurchaseError> {
    if new.amount_cents <= 0 {
        return Err(PurchaseError::InvalidAmount);
    }
    sqlx::query(
        "INSERT INTO purchase (id, person_id, email, offering, offering_name, cadence, \
         amount_cents, currency, account_code, state, inserted_at, updated_at, \
         stripe_firm_id, stripe_account_id, renewal_terms, renewal_consented_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'invoicing', $10, $10, $11, $12, $13, \
         CASE WHEN $12 IS NOT NULL THEN $10 ELSE NULL END) \
         ON CONFLICT (person_id, offering) WHERE state IN ('invoicing', 'invoiced') DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(new.person_id)
    .bind(&new.email)
    .bind(&new.offering)
    .bind(&new.offering_name)
    .bind(new.cadence.as_str())
    .bind(new.amount_cents)
    .bind(&new.currency)
    .bind(&new.account_code)
    .bind(now)
    .bind(stripe.as_ref().map(|s| s.firm_id))
    .bind(stripe.as_ref().map(|s| s.account_id))
    .bind(stripe.as_ref().map(|s| s.terms))
    .execute(pg.pool())
    .await?;
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE person_id = $1 AND offering = $2 \
         AND state IN ('invoicing', 'invoiced')"
    )))
    .bind(new.person_id)
    .bind(&new.offering)
    .fetch_one(pg.pool())
    .await?;
    from_row(&row)
}

/// Record a complimentary Purchase. It is never invoiced.
///
/// # Errors
/// A database error.
pub async fn record_complimentary(
    pg: &PostgresDb,
    new: &NewPurchase,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO purchase (id, person_id, email, offering, offering_name, cadence, \
         amount_cents, currency, account_code, state, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 0, $7, $8, 'complimentary', $9, $9) RETURNING {COLUMNS}"
    )))
    .bind(Uuid::new_v4())
    .bind(new.person_id)
    .bind(&new.email)
    .bind(&new.offering)
    .bind(&new.offering_name)
    .bind(new.cadence.as_str())
    .bind(&new.currency)
    .bind(&new.account_code)
    .bind(now)
    .fetch_one(pg.pool())
    .await?;
    from_row(&row)
}

/// Record the Xero invoice raised for an `invoicing` Purchase. A Purchase
/// already `invoiced` keeps the invoice it has.
///
/// # Errors
/// [`PurchaseError::NotFound`] when no open Purchase has this id.
pub async fn record_invoice(
    pg: &PostgresDb,
    id: Uuid,
    xero_contact_id: &str,
    xero_invoice_id: &str,
    invoice_url: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE purchase SET state = 'invoiced', xero_contact_id = COALESCE(xero_contact_id, $2), \
         xero_invoice_id = COALESCE(xero_invoice_id, $3), \
         invoice_url = COALESCE($4, invoice_url), updated_at = $5 \
         WHERE id = $1 AND state IN ('invoicing', 'invoiced') RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(xero_contact_id)
    .bind(xero_invoice_id)
    .bind(invoice_url)
    .bind(now)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(PurchaseError::NotFound)?;
    from_row(&row)
}

/// Turn an `invoiced` Purchase `active`, recording the repeating invoice that
/// renews a subscription. Activating an already active Purchase is a no-op
/// that returns it unchanged.
///
/// # Errors
/// [`PurchaseError::NotFound`] when the Purchase is neither invoiced nor
/// active.
pub async fn activate(
    pg: &PostgresDb,
    id: Uuid,
    xero_repeating_invoice_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE purchase SET state = 'active', activated_at = $2, \
         xero_repeating_invoice_id = $3, updated_at = $2 \
         WHERE id = $1 AND state = 'invoiced' RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(now)
    .bind(xero_repeating_invoice_id)
    .fetch_optional(pg.pool())
    .await?;
    match updated {
        Some(row) => from_row(&row),
        None => match get(pg, id).await? {
            purchase if purchase.state == PurchaseState::Active => Ok(purchase),
            _ => Err(PurchaseError::NotFound),
        },
    }
}

/// One Purchase by id.
///
/// # Errors
/// [`PurchaseError::NotFound`] or a database error.
pub async fn get(pg: &PostgresDb, id: Uuid) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(PurchaseError::NotFound)?;
    from_row(&row)
}

/// A Client's Purchases, newest first.
///
/// # Errors
/// A database error.
pub async fn for_person(pg: &PostgresDb, person_id: Uuid) -> Result<Vec<Purchase>, PurchaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE person_id = $1 ORDER BY inserted_at DESC, id"
    )))
    .bind(person_id)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Every Purchase of the given offerings, in every state, oldest first.
///
/// The monthly credit reset reads its subscriber list through this: an
/// `active` Purchase makes a subscriber, while an earlier-state Purchase
/// still names the contact an overage invoice bills to.
///
/// # Errors
/// A database error.
pub async fn for_offerings(
    pg: &PostgresDb,
    offerings: &[&str],
) -> Result<Vec<Purchase>, PurchaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE offering = ANY($1) \
         ORDER BY person_id, inserted_at, id"
    )))
    .bind(offerings)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Purchases awaiting payment, least recently checked first: the queue the
/// activation schedule drains.
///
/// # Errors
/// A database error.
pub async fn invoiced(pg: &PostgresDb, limit: i64) -> Result<Vec<Purchase>, PurchaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE state = 'invoiced' \
         ORDER BY updated_at ASC, id LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Note a payment refresh attempt so unpaid invoices and active subscriptions
/// rotate even when the provider refuses the refresh.
///
/// # Errors
/// A database error.
pub async fn touch(pg: &PostgresDb, id: Uuid, now: DateTime<Utc>) -> Result<(), PurchaseError> {
    sqlx::query("UPDATE purchase SET updated_at = $2 WHERE id = $1 AND (state = 'invoiced' OR (state = 'active' AND stripe_account_id IS NOT NULL))")
        .bind(id)
        .bind(now)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Persist hosted Checkout before redirecting; an open Purchase cannot change provider sessions.
pub async fn record_stripe_checkout(
    pg: &PostgresDb,
    id: Uuid,
    session: &str,
    url: &str,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE purchase SET state = 'invoiced', stripe_checkout_session_id = $2, invoice_url = $3, updated_at = $4 \
         WHERE id = $1 AND stripe_account_id IS NOT NULL AND state IN ('invoicing', 'invoiced') \
         AND (stripe_checkout_session_id IS NULL OR stripe_checkout_session_id = $2) RETURNING {COLUMNS}"
    ))).bind(id).bind(session).bind(url).bind(now).fetch_optional(pg.pool()).await?.ok_or(PurchaseError::NotFound)?;
    from_row(&row)
}

/// Only provider-verified identities and a confirmed paid-through date reach this write.
pub struct StripeObservation<'a> {
    pub customer: &'a str,
    pub subscription: &'a str,
    pub paid_through: DateTime<Utc>,
    pub cancelling: bool,
}

/// Apply a verified subscription observation without allowing a customer or subscription to change.
pub async fn sync_stripe(
    pg: &PostgresDb,
    id: Uuid,
    observation: StripeObservation<'_>,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE purchase SET state = 'active', activated_at = COALESCE(activated_at, $6), \
         stripe_customer_id = $2, stripe_subscription_id = $3, stripe_period_end = $4, \
         stripe_cancel_at_period_end = $5, updated_at = $6, \
         renewal_cancelled_at = CASE WHEN $5 THEN COALESCE(renewal_cancelled_at, $6) ELSE NULL END \
         WHERE id = $1 AND stripe_checkout_session_id IS NOT NULL \
         AND (stripe_customer_id IS NULL OR stripe_customer_id = $2) \
         AND (stripe_subscription_id IS NULL OR stripe_subscription_id = $3) RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(observation.customer)
    .bind(observation.subscription)
    .bind(observation.paid_through)
    .bind(observation.cancelling)
    .bind(now)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(PurchaseError::NotFound)?;
    from_row(&row)
}

/// Record a provider-confirmed stop of future renewal; retain invoices and engagement records.
pub async fn record_renewal_cancelled(
    pg: &PostgresDb,
    id: Uuid,
    now: DateTime<Utc>,
) -> Result<(), PurchaseError> {
    sqlx::query("UPDATE purchase SET renewal_cancelled_at = COALESCE(renewal_cancelled_at, $2), updated_at = $2 WHERE id = $1")
        .bind(id).bind(now).execute(pg.pool()).await?;
    Ok(())
}

/// Rotate pending payments and Stripe renewals by last attempt so one refusal cannot starve the queue.
pub async fn payment_refresh_queue(
    pg: &PostgresDb,
    limit: i64,
) -> Result<Vec<Purchase>, PurchaseError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM purchase WHERE state = 'invoiced' OR \
         (state = 'active' AND stripe_account_id IS NOT NULL) ORDER BY updated_at, id LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(pg.pool())
    .await?;
    rows.iter().map(from_row).collect()
}

/// Stop a Xero renewal without taking away the period its schedule already covers.
pub async fn record_xero_cancelled(
    pg: &PostgresDb,
    id: Uuid,
    end: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), PurchaseError> {
    sqlx::query("UPDATE purchase SET renewal_cancelled_at = COALESCE(renewal_cancelled_at, $3), access_ends_at = COALESCE(access_ends_at, $2), updated_at = $3 WHERE id = $1 AND stripe_account_id IS NULL")
        .bind(id).bind(end).bind(now).execute(pg.pool()).await?;
    Ok(())
}

/// Release an unpaid reservation only after its bound provider session is confirmed expired.
pub async fn expire_stripe_checkout(
    pg: &PostgresDb,
    id: Uuid,
    session: &str,
    now: DateTime<Utc>,
) -> Result<Purchase, PurchaseError> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE purchase SET state = 'expired', updated_at = $3 WHERE id = $1 \
         AND stripe_checkout_session_id = $2 AND stripe_subscription_id IS NULL \
         AND state IN ('invoiced', 'expired') RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(session)
    .bind(now)
    .fetch_optional(pg.pool())
    .await?
    .ok_or(PurchaseError::NotFound)?;
    from_row(&row)
}
