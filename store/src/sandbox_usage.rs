//! The metered-credit ledger behind the Nautilus Legal Sandbox.
//!
//! A Nautilus subscriber's Legal Sandbox includes $5 of model usage per
//! billing month; usage beyond that is priced à la carte. This module owns
//! the ledger that makes that promise enforceable: one row per subscriber
//! per billing month — the month's grant (500 cents), the running debit
//! across completions, and the overage flag. Money is integer cents, the
//! `billing` crate's convention; a float never enters this table.
//!
//! # The record id is the concurrency boundary
//!
//! One row per (subscriber, period) sounds like a UNIQUE index on the
//! pair. It is not, for the reason `store/tests/person_mailbox_race.rs`
//! demonstrated and `store::xero_invoices` re-learned: a UNIQUE index does
//! not serialize racers that each mint their own record id, because the
//! optimistic layer conflicts on record keys, not on index entries. The
//! row's own record id is therefore the compound natural key
//! `⟨<person uuid>:<period>⟩`, so a second `CREATE` for the same month
//! collides on that key and reports a typed
//! [`surrealdb::types::AlreadyExistsError::Record`] — which the optimistic
//! layer does serialize.
//!
//! The same reasoning puts the record address directly inside the debit.
//! A metered debit is an `UPDATE` of that one record whose `WHERE` holds
//! the whole decision (a scan predicate would never enter the read set),
//! so two parallel tabs of the same subscriber contend on the same record
//! key: the optimistic layer serializes them, and each debit applies
//! exactly once. What a read-then-write could lose — a completion charged
//! twice, or a debit silently dropped — the single conditional statement
//! cannot.
//!
//! # The crossing completion completes
//!
//! The Legal Sandbox's own rule: the completion that crosses zero succeeds
//! and is billed, and the next one waits for consent. A debit is therefore
//! refused only when the row is *already* flagged `overage`. The debit
//! that pushes the balance negative applies in full and sets the flag;
//! every debit after it reports [`MeterDecision::Exhausted`] until the à
//! la carte charge is settled. Spending to exactly zero is not crossing —
//! the next completion is the one that does.
//!
//! # Rollover never rewrites history
//!
//! A period's credit is a row of its own, created on first use of that
//! period (or by the monthly rollover job calling [`grant`], which
//! converges — an existing row is returned untouched, never re-granted).
//! A closed period's row is never rewritten or deleted, so an overage row
//! survives exactly as the overage-invoice job needs to read it: [`find`]
//! and [`overage_rows`] are the only reads it requires.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::{AlreadyExistsError, ErrorDetails, RecordId, SurrealValue};
use thiserror::Error;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The SurrealDB table holding the sandbox usage ledger.
pub(crate) const TABLE: &str = "sandbox_usage";
const SELECT: &str = "person_id, period, granted_cents, spent_cents, overage, \
                      inserted_at, updated_at";

const PERSON_TABLE: &str = "person";

/// The model-usage credit a Nautilus subscription includes for one billing
/// month: $5.00 in integer cents. The glossary's number, not a call-site
/// choice — every fresh row starts here and the rollover job grants it.
pub const MONTHLY_GRANT_CENTS: i64 = 500;

/// One subscriber's metered-credit row for one billing month.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`SandboxUsageRow`] is the seam that turns it into what the engine
/// reads and writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SandboxUsage {
    /// The subscriber this credit belongs to.
    pub person_id: Uuid,
    /// The billing month, spelled `YYYY-MM` — see [`billing_period`].
    pub period: String,
    /// The month's granted credit in cents (500 for the contract grant).
    pub granted_cents: i64,
    /// The running debit across the month's completions, in cents.
    pub spent_cents: i64,
    /// True once a completion pushed the balance negative: the subscriber
    /// owes à la carte charges for the overage, and further completions
    /// wait for consent.
    pub overage: bool,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SandboxUsage {
    /// Credit left in the period, in cents. Negative only on the row that
    /// crossed — the completion that pushed the balance negative was
    /// billed in full, by the glossary's rule.
    #[must_use]
    pub fn remaining_cents(&self) -> i64 {
        self.granted_cents - self.spent_cents
    }
}

/// What the ledger decided when asked to charge one completion's price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeterDecision {
    /// The completion is charged and may proceed. `remaining_cents` is the
    /// balance after this charge — negative on the charge that crossed.
    Charge {
        /// The row's total debit after this charge, in cents.
        spent_cents: i64,
        /// The balance after this charge, in cents.
        remaining_cents: i64,
    },
    /// The balance is already past its credit: block the completion and
    /// surface the à la carte offer instead of a silent failure. A failed
    /// completion is never charged — the caller only debits on success.
    Exhausted,
}

/// Errors reading or writing the sandbox usage ledger.
///
/// This is deliberately not a bare [`surrealdb::Error`]: `web`, `webapp`,
/// and `billing-workflows` consume this module without depending on the
/// SurrealDB crate.
#[derive(Debug, Error)]
pub enum SandboxUsageError {
    /// A database operation failed.
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A write claimed success but returned no usable row.
    #[error("writing the sandbox usage ledger returned no usable row")]
    WriteReturnedNothing,
    /// A period that is not a `YYYY-MM` billing month. A misspelled
    /// period would silently fork a subscriber's credit across two
    /// spellings of one month, so it is refused rather than stored.
    #[error("invalid billing period {0:?}: expected `YYYY-MM`")]
    InvalidPeriod(String),
    /// A debit priced below zero — a credit refund smuggled through the
    /// meter. Charges are never negative.
    #[error("a sandbox usage debit cannot be negative ({0} cents)")]
    NegativePrice(i64),
}

/// The one way to spell a billing month: `YYYY-MM`.
///
/// The rollover job and the gateway both build periods through this so a
/// stray format can never fork a subscriber's credit across two spellings
/// of one month. The ledger's entry points refuse anything else.
#[must_use]
pub fn billing_period(year: i32, month: u32) -> String {
    format!("{year:04}-{month:02}")
}

/// Whether `period` is a billing month in [`billing_period`]'s shape.
fn is_valid_period(period: &str) -> bool {
    let Some((year, month)) = period.split_once('-') else {
        return false;
    };
    year.len() == 4
        && month.len() == 2
        && year.bytes().all(|b| b.is_ascii_digit())
        && month.bytes().all(|b| b.is_ascii_digit())
        && month.parse::<u8>().is_ok_and(|m| (1..=12).contains(&m))
}

fn require_period(period: &str) -> Result<(), SandboxUsageError> {
    is_valid_period(period)
        .then_some(())
        .ok_or_else(|| SandboxUsageError::InvalidPeriod(period.to_string()))
}

/// The row's own record id: the compound natural key
/// `⟨<person uuid>:<period>⟩`. Neither half contains a `:` (a UUID never
/// does; a validated period never does), so the spelling is one-to-one —
/// but nothing parses it back: the fields are the truth, the key is only
/// the collision point.
fn record_for(person_id: Uuid, period: &str) -> RecordId {
    RecordId::new(TABLE, format!("{person_id}:{period}"))
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct SandboxUsageRow {
    person_id: RecordId,
    period: String,
    granted_cents: i64,
    spent_cents: i64,
    overage: bool,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl SandboxUsageRow {
    /// `None` when `person_id` is not a native UUID key. Navigator only
    /// writes native UUID ids through [`record_id`], so reporting another
    /// shape would invent an id this module cannot faithfully represent.
    fn into_usage(self) -> Option<SandboxUsage> {
        Some(SandboxUsage {
            person_id: record_uuid(&self.person_id)?,
            period: self.period,
            granted_cents: self.granted_cents,
            spent_cents: self.spent_cents,
            overage: self.overage,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

fn one(mut response: surrealdb::IndexedResults) -> Result<Option<SandboxUsage>, SandboxUsageError> {
    let row: Option<SandboxUsageRow> = response.take(0)?;
    Ok(row.and_then(SandboxUsageRow::into_usage))
}

fn many(mut response: surrealdb::IndexedResults) -> Result<Vec<SandboxUsage>, SandboxUsageError> {
    let rows: Vec<SandboxUsageRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(SandboxUsageRow::into_usage)
        .collect())
}

/// Run a write under the shared retry policy
/// ([`crate::surreal::retry`]), mapping whatever finally comes back to
/// this module's error.
///
/// Only the mapping lives here. How long a lost race is re-run, and
/// which engine conditions count as a lost race, are one policy for the
/// whole crate.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, SandboxUsageError>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await.map_err(SandboxUsageError::Db)
}

/// Whether `error` is a second `CREATE` colliding with the row this
/// (subscriber, period) already has.
///
/// The collision is **typed**: `CREATE` onto a taken record id reports
/// [`AlreadyExistsError::Record`] carrying that id, so the discriminator
/// is a structured value rather than prose — the same shape
/// `store::persons` and `store::xero_invoices` match on.
fn row_taken(error: &surrealdb::Error) -> bool {
    matches!(
        error.details(),
        ErrorDetails::AlreadyExists(Some(AlreadyExistsError::Record { id }))
            if id.starts_with(TABLE)
    )
}

/// The row for one subscriber's one billing month, or `None` when that
/// period has no row yet.
///
/// # Errors
///
/// [`SandboxUsageError::Db`] when SurrealDB cannot read the ledger.
pub async fn find(
    db: &SurrealDb,
    person_id: Uuid,
    period: &str,
) -> Result<Option<SandboxUsage>, SandboxUsageError> {
    require_period(period)?;
    let response = db
        .query(format!("SELECT {SELECT} FROM ONLY $id"))
        .bind(("id", record_for(person_id, period)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    one(response)
}

/// Create the period's row with the contract grant. `Ok(None)` when
/// another writer's create won the race for the same compound key — the
/// row exists either way, so a lost create is not a fault; the caller
/// re-reads it rather than reporting one.
async fn create(
    db: &SurrealDb,
    person_id: Uuid,
    period: &str,
) -> Result<Option<SandboxUsage>, SandboxUsageError> {
    let attempt = || {
        db.query(format!(
            "CREATE type::record('{TABLE}', $key) SET \
                 person_id = $person_id, period = $period, \
                 granted_cents = $grant, spent_cents = 0, overage = false, \
                 inserted_at = time::now(), updated_at = time::now() \
             RETURN {SELECT}"
        ))
        .bind(("key", format!("{person_id}:{period}")))
        .bind(("person_id", record_id(PERSON_TABLE, person_id)))
        .bind(("period", period.to_string()))
        .bind(("grant", MONTHLY_GRANT_CENTS))
    };
    match retry::writing(attempt).await {
        Ok(response) => one(response),
        Err(error) if row_taken(&error) => Ok(None),
        Err(error) => Err(SandboxUsageError::Db(error)),
    }
}

/// Idempotently grant one subscriber the billing month's credit: the
/// primitive the monthly rollover job calls for each active subscriber.
///
/// A period with no row yet gets one carrying the contract grant
/// ([`MONTHLY_GRANT_CENTS`]). An existing row is returned **untouched** —
/// a re-run converges instead of re-granting or resetting a subscriber's
/// spent credit. The row this creates is keyed on the compound natural
/// key, so a rollover racing a subscriber's first completion settles on
/// the one row rather than forking the credit in two.
///
/// # Errors
///
/// [`SandboxUsageError::InvalidPeriod`] when `period` is not a `YYYY-MM`
/// billing month; [`SandboxUsageError::Db`] when SurrealDB cannot read or
/// write the ledger; [`SandboxUsageError::WriteReturnedNothing`] when a
/// lost create race is followed by a row that cannot be read back.
pub async fn grant(
    db: &SurrealDb,
    person_id: Uuid,
    period: &str,
) -> Result<SandboxUsage, SandboxUsageError> {
    require_period(period)?;
    if let Some(row) = find(db, person_id, period).await? {
        return Ok(row);
    }
    match create(db, person_id, period).await? {
        Some(created) => Ok(created),
        // Another writer granted this period first; its row is the truth.
        None => find(db, person_id, period)
            .await?
            .ok_or(SandboxUsageError::WriteReturnedNothing),
    }
}

/// The one statement that applies a debit. `overage` is assigned before
/// `spent_cents` is touched, so every `spent_cents` reference in both
/// expressions reads the pre-debit balance whichever way the engine
/// resolves intra-statement field references — the price is counted once.
///
/// The `WHERE` is the whole decision: a row already flagged `overage`
/// updates to nothing (an empty result set), a row still inside its
/// credit charges in full. Addressed at the record directly, so two
/// parallel debits contend on the record key and the optimistic layer
/// serializes them.
async fn conditional_debit(
    db: &SurrealDb,
    person_id: Uuid,
    period: &str,
    price_cents: i64,
) -> Result<surrealdb::IndexedResults, SandboxUsageError> {
    writing(|| {
        db.query(format!(
            "UPDATE $id SET \
                 overage = (spent_cents + $price) > granted_cents, \
                 spent_cents = spent_cents + $price, \
                 updated_at = time::now() \
             WHERE overage = false \
             RETURN {SELECT}"
        ))
        .bind(("id", record_for(person_id, period)))
        .bind(("price", price_cents))
    })
    .await
}

/// Charge one completion's price against a subscriber's billing month.
///
/// The period's row materializes on first use — the rollover job normally
/// granted it already, but a subscriber's first completion of a month
/// must never fail because a job has not run. The debit then applies as
/// the one conditional statement ([`conditional_debit`]): a charge that
/// fits inside the remaining credit proceeds normally; the charge that
/// crosses zero **also proceeds** — the crossing completion is billed in
/// full and flags the row `overage`; a row already past its credit
/// reports [`MeterDecision::Exhausted`] and the caller surfaces the à la
/// carte offer instead of a silent failure.
///
/// Callers debit only completions that succeeded: a failed completion is
/// never charged.
///
/// # Errors
///
/// [`SandboxUsageError::NegativePrice`] when `price_cents` is negative;
/// [`SandboxUsageError::InvalidPeriod`] when `period` is not a `YYYY-MM`
/// billing month; [`SandboxUsageError::Db`] when SurrealDB cannot read or
/// write the ledger.
pub async fn debit(
    db: &SurrealDb,
    person_id: Uuid,
    period: &str,
    price_cents: i64,
) -> Result<MeterDecision, SandboxUsageError> {
    if price_cents < 0 {
        return Err(SandboxUsageError::NegativePrice(price_cents));
    }
    require_period(period)?;

    if find(db, person_id, period).await?.is_none() {
        create(db, person_id, period).await?;
    }

    let response = conditional_debit(db, person_id, period, price_cents).await?;
    match one(response)? {
        Some(row) => Ok(MeterDecision::Charge {
            spent_cents: row.spent_cents,
            remaining_cents: row.remaining_cents(),
        }),
        // The row exists — it was found or created above — and the only
        // way the predicate fails is a row already past its credit.
        None => Ok(MeterDecision::Exhausted),
    }
}

/// Every row of `period` that owes à la carte charges — the read the
/// overage-invoice job consumes after the month closes. Ordered by
/// subscriber for a deterministic invoice batch.
///
/// # Errors
///
/// [`SandboxUsageError::InvalidPeriod`] when `period` is not a `YYYY-MM`
/// billing month; [`SandboxUsageError::Db`] when SurrealDB cannot read
/// the ledger.
pub async fn overage_rows(
    db: &SurrealDb,
    period: &str,
) -> Result<Vec<SandboxUsage>, SandboxUsageError> {
    require_period(period)?;
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} \
             WHERE period = $period AND overage = true ORDER BY person_id"
        ))
        .bind(("period", period.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    many(response)
}
