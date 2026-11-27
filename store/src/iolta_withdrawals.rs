//! One bank transfer out of a state's pooled IOLTA account, and how it splits
//! across the invoices it settles.
//!
//! # Why a split is modelled rather than inferred
//!
//! The bank sees one transfer. A $10,000 withdrawal from the Nevada pool may
//! settle four matters' invoices at once, and each of those clients is
//! entitled to see how *their* money left trust — and to see nothing about
//! the other three. So the transfer is mirrored as one
//! [`IoltaWithdrawal`] plus one [`IoltaAllocation`] per invoice it settles,
//! and a client's matter page renders only the lines carrying their
//! `project_id`. The pooled total is a firm-side number; it never reaches a
//! client surface.
//!
//! # All or nothing
//!
//! [`apply`] refuses a withdrawal that does not hold together, and refuses it
//! **whole**: nothing is written, no draw is posted, and the caller reports
//! it for a human. A withdrawal is refused when
//!
//! * its lines do not sum to exactly the transfer
//!   ([`IoltaWithdrawalError::LinesDoNotSum`]) — a partially explained
//!   transfer would leave the pool and the ledgers disagreeing;
//! * a line names an invoice no mirror row carries
//!   ([`IoltaWithdrawalError::UnknownInvoice`]);
//! * a line's matter does not sit on the state pool the money left
//!   ([`IoltaWithdrawalError::WrongPool`]);
//! * a matter's lines exceed what that matter actually holds
//!   ([`IoltaWithdrawalError::Overdraw`]). Drawing more than a client has in
//!   trust is the error trust accounting exists to prevent, so it is never
//!   clipped to what fits — the numbers are wrong and a human reconciles
//!   them.
//!
//! # What lands when it does hold together
//!
//! The withdrawal row, its allocation lines, and one
//! [`crate::trust::MovementKind::EarnedDraw`] per matter for the total of
//! that matter's lines, carrying the Xero transaction id as its
//! `external_ref`. The draw is what moves the matter's `held_cents` down;
//! the allocations are what let the client see which invoices it went to.
//! Re-reading the same transfer on a later night posts nothing new.
//!
//! Navigator never creates the transfer in Xero. This projection mirrors Xero;
//! the authoritative Firm journal lives in `crate::firm_journal`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::accounting as authority;

/// One mirrored transfer out of a state's pooled account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IoltaWithdrawal {
    /// Xero `BankTransactionID`, and this row's own record id.
    pub xero_transaction_id: String,
    /// The state pool the money left.
    pub jurisdiction_id: Uuid,
    /// The whole transfer, minor units. Firm-side: never rendered to a
    /// client, who is shown only their own lines.
    pub total_cents: i64,
    pub currency: String,
    pub occurred_at: DateTime<Utc>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One line of the split: this much of the transfer settled this invoice for
/// this matter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IoltaAllocation {
    /// The withdrawal this line belongs to.
    pub withdrawal_id: String,
    /// The Xero invoice it settles. Internal — never rendered to a client.
    pub xero_invoice_id: String,
    /// The matter billed by that invoice, copied from the invoice mirror so
    /// a line cannot name a matter its invoice does not bill.
    pub project_id: Uuid,
    /// The invoice's human reference, which is what a client is shown.
    pub invoice_reference: String,
    pub amount_cents: i64,
    pub occurred_at: DateTime<Utc>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The withdrawal row as PostgreSQL's `to_jsonb` serves it. The row's `id`
/// is the Xero transaction id itself, so it adds nothing to decode.
#[derive(serde::Deserialize)]
struct PgWithdrawalRow {
    xero_transaction_id: String,
    jurisdiction_id: Uuid,
    total_cents: i64,
    currency: String,
    occurred_at: DateTime<Utc>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgWithdrawalRow> for IoltaWithdrawal {
    fn from(row: PgWithdrawalRow) -> Self {
        IoltaWithdrawal {
            xero_transaction_id: row.xero_transaction_id,
            jurisdiction_id: row.jurisdiction_id,
            total_cents: row.total_cents,
            currency: row.currency,
            occurred_at: row.occurred_at,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// The allocation row as PostgreSQL's `to_jsonb` serves it. The row's `id`
/// is the `withdrawal--invoice` pair, so it adds nothing to decode.
#[derive(serde::Deserialize)]
struct PgAllocationRow {
    withdrawal_id: String,
    xero_invoice_id: String,
    project_id: Uuid,
    invoice_reference: String,
    amount_cents: i64,
    occurred_at: DateTime<Utc>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgAllocationRow> for IoltaAllocation {
    fn from(row: PgAllocationRow) -> Self {
        IoltaAllocation {
            withdrawal_id: row.withdrawal_id,
            xero_invoice_id: row.xero_invoice_id,
            project_id: row.project_id,
            invoice_reference: row.invoice_reference,
            amount_cents: row.amount_cents,
            occurred_at: row.occurred_at,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// Why a withdrawal was refused. Every variant names the numbers a human
/// needs to reconcile it in Xero — none of them is recoverable by guessing.
#[derive(Debug, Error)]
pub enum IoltaWithdrawalError {
    /// Boxed, like its sibling below: these two carry the largest payloads
    /// in this enum, and every function here returns it — an unboxed variant
    /// would widen every `Ok` on the happy path too.
    #[error(transparent)]
    Invoices(Box<crate::xero_invoices::XeroInvoiceError>),
    #[error(transparent)]
    Accounts(Box<crate::iolta_accounts::IoltaAccountError>),
    /// The transfer left a pool no mirrored account matches.
    #[error("no mirrored IOLTA account for Xero account {0}")]
    NoSuchPool(String),
    /// A withdrawal with no lines explains nothing.
    #[error("withdrawal {transaction_id} allocates nothing")]
    NoAllocations { transaction_id: String },
    /// The lines do not add up to the transfer.
    #[error(
        "withdrawal {transaction_id} moved {total_cents} cents but its lines allocate \
         {allocated_cents}"
    )]
    LinesDoNotSum {
        transaction_id: String,
        total_cents: i64,
        allocated_cents: i64,
    },
    /// A line names an invoice the mirror does not carry, so which matter it
    /// settles is unknown.
    #[error("withdrawal {transaction_id} allocates to unmirrored invoice {invoice_reference}")]
    UnknownInvoice {
        transaction_id: String,
        invoice_reference: String,
    },
    /// The money left one state's pool but a line settles a matter governed
    /// by another state — or by no state at all.
    #[error(
        "withdrawal {transaction_id} left the pool for jurisdiction {jurisdiction_id}, but \
         matter {project_id} does not sit on it"
    )]
    WrongPool {
        transaction_id: String,
        jurisdiction_id: Uuid,
        project_id: Uuid,
    },
    /// A matter's lines exceed what it holds in trust. Refused whole: the
    /// numbers are wrong, and clipping the draw to what fits would hide it.
    #[error(
        "withdrawal {transaction_id} draws {requested_cents} cents for matter {project_id}, \
         which holds {held_cents}"
    )]
    Overdraw {
        transaction_id: String,
        project_id: Uuid,
        held_cents: i64,
        requested_cents: i64,
    },
    /// Reading or writing the matter's trust ledger failed.
    #[error("trust ledger: {0}")]
    Trust(String),
    /// PostgreSQL, under the accounting authority, failed.
    #[error(transparent)]
    Postgres(#[from] crate::postgres::accounting::AccountingAuthorityError),
    #[error("writing an IOLTA withdrawal returned no usable row")]
    WriteReturnedNothing,
}

impl From<crate::xero_invoices::XeroInvoiceError> for IoltaWithdrawalError {
    fn from(error: crate::xero_invoices::XeroInvoiceError) -> Self {
        Self::Invoices(Box::new(error))
    }
}

impl From<crate::iolta_accounts::IoltaAccountError> for IoltaWithdrawalError {
    fn from(error: crate::iolta_accounts::IoltaAccountError) -> Self {
        Self::Accounts(Box::new(error))
    }
}

/// One line of a withdrawal as the mirror reads it from Xero: how much of
/// the transfer settled which invoice.
#[derive(Clone, Debug)]
pub struct AllocationInput {
    /// The invoice reference carried on the Xero line
    /// (`store::xero_invoices::XeroInvoice::reference`, or the Xero invoice
    /// id itself).
    pub invoice_reference: String,
    pub amount_cents: i64,
}

/// One transfer out of a pooled account, with its split.
#[derive(Clone, Debug)]
pub struct WithdrawalInput {
    /// Xero `BankTransactionID`.
    pub xero_transaction_id: String,
    /// The Xero bank account the money left, matched to a mirrored pool.
    pub xero_account_id: String,
    pub total_cents: i64,
    pub currency: String,
    pub occurred_at: DateTime<Utc>,
    pub lines: Vec<AllocationInput>,
}

/// What [`apply`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The withdrawal, its lines, and one earned draw per matter landed.
    /// Carries the matters drawn against, for the caller's report.
    Posted { projects: Vec<Uuid> },
    /// This Xero transfer was already mirrored on an earlier night. Nothing
    /// was written and no draw was posted again.
    AlreadyApplied,
}

/// Mirror one pooled withdrawal: validate the whole split, write the
/// withdrawal and its lines, and post one earned draw per matter.
///
/// Idempotent on the Xero transaction id. See the module docs for the four
/// ways a withdrawal is refused — all of them refuse it whole.
///
/// # Errors
///
/// Any [`IoltaWithdrawalError`]. A refusal writes nothing.
pub async fn apply(
    db: &ApplicationStore,
    input: &WithdrawalInput,
) -> Result<Applied, IoltaWithdrawalError> {
    if find(db, &input.xero_transaction_id).await?.is_some() {
        return Ok(Applied::AlreadyApplied);
    }
    let pool = crate::iolta_accounts::all(db)
        .await?
        .into_iter()
        .find(|account| account.xero_account_id == input.xero_account_id)
        .ok_or_else(|| IoltaWithdrawalError::NoSuchPool(input.xero_account_id.clone()))?;

    if input.lines.is_empty() {
        return Err(IoltaWithdrawalError::NoAllocations {
            transaction_id: input.xero_transaction_id.clone(),
        });
    }
    let allocated_cents: i64 = input.lines.iter().map(|line| line.amount_cents).sum();
    if allocated_cents != input.total_cents {
        return Err(IoltaWithdrawalError::LinesDoNotSum {
            transaction_id: input.xero_transaction_id.clone(),
            total_cents: input.total_cents,
            allocated_cents,
        });
    }

    // Resolve every line to the invoice it settles, and through the invoice
    // to the matter it bills. The matter is never taken from the line
    // itself, so a line cannot claim an invoice belongs to another matter.
    let mut resolved = Vec::with_capacity(input.lines.len());
    for line in &input.lines {
        let invoice = crate::xero_invoices::find_for_allocation(db, &line.invoice_reference)
            .await?
            .ok_or_else(|| IoltaWithdrawalError::UnknownInvoice {
                transaction_id: input.xero_transaction_id.clone(),
                invoice_reference: line.invoice_reference.clone(),
            })?;
        let matter_pool = crate::iolta_accounts::for_project(db, invoice.project_id).await?;
        if matter_pool.map(|account| account.jurisdiction_id) != Some(pool.jurisdiction_id) {
            return Err(IoltaWithdrawalError::WrongPool {
                transaction_id: input.xero_transaction_id.clone(),
                jurisdiction_id: pool.jurisdiction_id,
                project_id: invoice.project_id,
            });
        }
        resolved.push((invoice, line.amount_cents));
    }

    // Every matter's whole share of this transfer, checked against what that
    // matter actually holds — before anything is written.
    let mut per_project: Vec<(Uuid, i64)> = Vec::new();
    for (invoice, amount_cents) in &resolved {
        match per_project
            .iter_mut()
            .find(|(project_id, _)| *project_id == invoice.project_id)
        {
            Some((_, total)) => *total += *amount_cents,
            None => per_project.push((invoice.project_id, *amount_cents)),
        }
    }
    for (project_id, requested_cents) in &per_project {
        let held_cents = crate::trust::position_for_project(db, *project_id)
            .await
            .map_err(IoltaWithdrawalError::Trust)?
            .held_cents();
        if *requested_cents > held_cents {
            return Err(IoltaWithdrawalError::Overdraw {
                transaction_id: input.xero_transaction_id.clone(),
                project_id: *project_id,
                held_cents,
                requested_cents: *requested_cents,
            });
        }
    }

    write_withdrawal(db, input, pool.jurisdiction_id).await?;
    for (invoice, amount_cents) in &resolved {
        write_allocation(db, input, invoice, *amount_cents).await?;
    }
    let occurred_at = input.occurred_at.to_rfc3339();
    for (project_id, cents) in &per_project {
        let draw = crate::trust::Movement::earned_draw(*project_id, *cents, occurred_at.clone())
            .with_external_ref(input.xero_transaction_id.clone());
        crate::trust::record_project_movement(db, *project_id, &draw)
            .await
            .map_err(IoltaWithdrawalError::Trust)?;
    }
    Ok(Applied::Posted {
        projects: per_project.into_iter().map(|(id, _)| id).collect(),
    })
}

pub(crate) async fn write_withdrawal(
    _db: &ApplicationStore,
    input: &WithdrawalInput,
    jurisdiction_id: Uuid,
) -> Result<(), IoltaWithdrawalError> {
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": input.xero_transaction_id,
        "xero_transaction_id": input.xero_transaction_id,
        "jurisdiction_id": jurisdiction_id, "total_cents": input.total_cents,
        "currency": input.currency, "occurred_at": input.occurred_at,
    });
    let query = sqlx::query(
        "INSERT INTO iolta_withdrawal SELECT * FROM \
         jsonb_populate_record(NULL::iolta_withdrawal, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (id) DO UPDATE SET jurisdiction_id = EXCLUDED.jurisdiction_id, \
         total_cents = EXCLUDED.total_cents, currency = EXCLUDED.currency, \
         occurred_at = EXCLUDED.occurred_at, updated_at = now()",
    )
    .bind(row);
    authority::execute(&pg, query).await?;
    Ok(())
}

pub(crate) async fn write_allocation(
    _db: &ApplicationStore,
    input: &WithdrawalInput,
    invoice: &crate::xero_invoices::XeroInvoice,
    amount_cents: i64,
) -> Result<(), IoltaWithdrawalError> {
    // The record id is the (withdrawal, invoice) pair, so a re-read of one
    // transfer refreshes its lines instead of doubling a matter's drawn
    // total — the same primary-key discipline the invoice mirror uses.
    let line_id = format!("{}--{}", input.xero_transaction_id, invoice.xero_invoice_id);
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": line_id, "withdrawal_id": input.xero_transaction_id,
        "xero_invoice_id": invoice.xero_invoice_id,
        "project_id": invoice.project_id,
        "invoice_reference": invoice.reference,
        "amount_cents": amount_cents, "occurred_at": input.occurred_at,
    });
    let query = sqlx::query(
        "INSERT INTO iolta_allocation SELECT * FROM \
         jsonb_populate_record(NULL::iolta_allocation, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (id) DO UPDATE SET invoice_reference = EXCLUDED.invoice_reference, \
         amount_cents = EXCLUDED.amount_cents, occurred_at = EXCLUDED.occurred_at, \
         updated_at = now()",
    )
    .bind(row);
    authority::execute(&pg, query).await?;
    Ok(())
}

/// The mirrored withdrawal for one Xero transaction id, if any.
///
/// # Errors
///
/// [`IoltaWithdrawalError::Db`] when the lookup fails.
pub async fn find(
    _db: &ApplicationStore,
    xero_transaction_id: &str,
) -> Result<Option<IoltaWithdrawal>, IoltaWithdrawalError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(iolta_withdrawal) AS row FROM iolta_withdrawal \
         WHERE xero_transaction_id = $1 LIMIT 1",
    )
    .bind(xero_transaction_id);
    Ok(authority::maybe::<PgWithdrawalRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// Every allocation line belonging to one matter, newest first.
///
/// **This is the client read.** It returns only the lines carrying
/// `project_id`, so a client sees how their own funds left trust and nothing
/// of the pooled transfer or of any other matter's share.
///
/// # Errors
///
/// [`IoltaWithdrawalError::Db`] when the lookup fails.
pub async fn for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Vec<IoltaAllocation>, IoltaWithdrawalError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(iolta_allocation) AS row FROM iolta_allocation \
         WHERE project_id = $1 ORDER BY occurred_at DESC",
    )
    .bind(project_id);
    Ok(authority::rows::<PgAllocationRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// Every line of one withdrawal — the firm-side read of how a pooled
/// transfer split. Never rendered on a client surface.
///
/// # Errors
///
/// [`IoltaWithdrawalError::Db`] when the lookup fails.
pub async fn lines_for(
    _db: &ApplicationStore,
    withdrawal_id: &str,
) -> Result<Vec<IoltaAllocation>, IoltaWithdrawalError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(iolta_allocation) AS row FROM iolta_allocation \
         WHERE withdrawal_id = $1 ORDER BY amount_cents DESC",
    )
    .bind(withdrawal_id);
    Ok(authority::rows::<PgAllocationRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[cfg(test)]
mod tests {}
