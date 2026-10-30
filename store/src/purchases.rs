//! A Client's Purchase of an Offering.
//!
//! The Offering, its price, and its cadence are copied from the services
//! catalog when the Purchase is made, so a later price change never rewrites
//! what a Client was invoiced. Xero raises and collects the invoice; this row
//! records which invoice answers for the Purchase and turns
//! [`PurchaseState::Active`] only once Xero reports that invoice paid.
//!
//! A Purchase moves `invoicing` → `invoiced` → `active`. `invoicing` exists so
//! the row is durable *before* Xero is called: the Purchase id is the Xero
//! idempotency key, so a retry after a lost answer finds the same open row and
//! asks Xero again with the same key rather than raising a second invoice.
//! A complimentary consultation is recorded as `complimentary` and never
//! touches Xero.

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
    /// Recorded; the Xero invoice is being raised.
    Invoicing,
    /// Xero holds an unpaid invoice for it.
    Invoiced,
    /// Xero reports the invoice paid; the Offering is in force.
    Active,
}

impl PurchaseState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complimentary => "complimentary",
            Self::Invoicing => "invoicing",
            Self::Invoiced => "invoiced",
            Self::Active => "active",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "complimentary" => Self::Complimentary,
            "invoicing" => Self::Invoicing,
            "invoiced" => Self::Invoiced,
            _ => Self::Active,
        }
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
     inserted_at, updated_at, activated_at";

/// The offerings whose Purchase is a Neon Law Nautilus subscription: the one
/// subscription sold under two names, DeleteYourData.com and Nautilus. The
/// canonical catalog anchor — the portal's subscription gate and the monthly
/// sandbox credit reset both key on these ids, so they live next to the
/// Purchase model rather than in either caller.
pub const NAUTILUS_OFFERINGS: [&str; 2] = ["delete-your-data-monthly", "delete-your-data-annual"];

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
        state: PurchaseState::parse(row.try_get("state")?),
        xero_contact_id: row.try_get("xero_contact_id")?,
        xero_invoice_id: row.try_get("xero_invoice_id")?,
        invoice_url: row.try_get("invoice_url")?,
        xero_repeating_invoice_id: row.try_get("xero_repeating_invoice_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
        activated_at: row.try_get("activated_at")?,
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
    if new.amount_cents <= 0 {
        return Err(PurchaseError::InvalidAmount);
    }
    sqlx::query(
        "INSERT INTO purchase (id, person_id, email, offering, offering_name, cadence, \
         amount_cents, currency, account_code, state, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'invoicing', $10, $10) \
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

/// Note that an invoiced Purchase was checked and is still unpaid, so the
/// queue rotates rather than re-reading the same invoices first.
///
/// # Errors
/// A database error.
pub async fn touch(pg: &PostgresDb, id: Uuid, now: DateTime<Utc>) -> Result<(), PurchaseError> {
    sqlx::query("UPDATE purchase SET updated_at = $2 WHERE id = $1 AND state = 'invoiced'")
        .bind(id)
        .bind(now)
        .execute(pg.pool())
        .await?;
    Ok(())
}
