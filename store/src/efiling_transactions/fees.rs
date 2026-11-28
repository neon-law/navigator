//! Accepted-envelope fee observations and one-charge settlement.

use chrono::NaiveDate;
use model::documents::{ExpenseCategory, ReceiptMetadata};
use serde::Serialize;
use sqlx::{postgres::PgRow, FromRow, Postgres, Row, Transaction};
use uuid::Uuid;

use super::{EfilingError, EfilingTransaction, STATUS_ACCEPTED};
use crate::{postgres::PostgresDb, project_costs as costs};

/// A filing fee remains pending until it points to a confirmed Project Cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeeObservation {
    pub receipt_document_id: Uuid,
    pub amount_cents: i64,
    pub currency: String,
    pub status: &'static str,
    pub cost_id: Option<Uuid>,
}

impl FromRow<'_, PgRow> for FeeObservation {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        let cost_id: Option<Uuid> = row.try_get("cost_id")?;
        Ok(Self {
            receipt_document_id: row.try_get("receipt_document_id")?,
            amount_cents: row.try_get("amount_cents")?,
            currency: row.try_get("currency")?,
            status: if cost_id.is_some() {
                "settled"
            } else {
                "pending"
            },
            cost_id,
        })
    }
}

#[derive(Serialize)]
pub struct FilingWithFee {
    #[serde(flatten)]
    pub filing: EfilingTransaction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee: Option<FeeObservation>,
}

const FEE_COLUMNS: &str = "fee_receipt_document_id AS receipt_document_id,
    fee_amount_cents AS amount_cents, fee_currency AS currency, fee_cost_id AS cost_id";

fn invalid(reason: &'static str) -> EfilingError {
    EfilingError::Invalid(reason.into())
}

/// Read available observations within an already authorized Project. The
/// Mercury account, source and reviewer remain in private settlement evidence.
pub async fn observations(
    project_id: Uuid,
) -> Result<std::collections::HashMap<Uuid, FeeObservation>, EfilingError> {
    let pg = super::matter::authority_for(super::TABLE).await?;
    let query = sqlx::AssertSqlSafe(format!(
        "SELECT id, {FEE_COLUMNS} FROM efiling_transactions
         WHERE project_id=$1 AND fee_receipt_document_id IS NOT NULL",
    ));
    let rows = sqlx::query(query)
        .bind(project_id)
        .fetch_all(pg.pool())
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            Ok((
                row.try_get::<Uuid, _>("id")?,
                FeeObservation::from_row(row)?,
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?)
}

async fn receipt(
    tx: &mut Transaction<'_, Postgres>,
    project_id: Uuid,
    receipt_id: Uuid,
    envelope: &str,
) -> Result<ReceiptMetadata, EfilingError> {
    let metadata: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT metadata FROM asset WHERE id=$1 AND project_id=$2 AND kind='receipt'
         AND visibility='internal' AND deleted_at IS NULL AND purged_at IS NULL FOR SHARE",
    )
    .bind(receipt_id)
    .bind(project_id)
    .fetch_optional(&mut **tx)
    .await?
    .flatten();
    let receipt = ReceiptMetadata::from_asset_metadata("receipt", metadata.as_ref())
        .ok()
        .flatten()
        .filter(|receipt| {
            receipt.transaction_id.as_deref() == Some(envelope)
                && matches!(
                    receipt.category,
                    ExpenseCategory::CourtFilingFee | ExpenseCategory::EFilingServiceFee
                )
        })
        .ok_or_else(|| {
            invalid("a live internal filing-fee receipt on this Project must name this envelope")
        })?;
    Ok(receipt)
}

/// Record one immutable receipt/amount observation under the Project lock.
pub(super) async fn observe(
    tx: &mut Transaction<'_, Postgres>,
    filing: &EfilingTransaction,
    receipt_id: Uuid,
) -> Result<FeeObservation, EfilingError> {
    if filing.status != STATUS_ACCEPTED {
        return Err(invalid("a fee observation requires an accepted envelope"));
    }
    let receipt = receipt(tx, filing.project_id, receipt_id, &filing.transaction_id).await?;
    let query = sqlx::AssertSqlSafe(format!(
        "UPDATE efiling_transactions SET fee_receipt_document_id=$1, fee_amount_cents=$2, fee_currency=$3
         WHERE id=$4 AND (fee_receipt_document_id IS NULL OR
           (fee_receipt_document_id=$1 AND fee_amount_cents=$2 AND fee_currency=$3))
         RETURNING {FEE_COLUMNS}",
    ));
    sqlx::query_as(query)
        .bind(receipt_id)
        .bind(receipt.amount_cents)
        .bind(receipt.currency)
        .bind(filing.id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|e| e.constraint() == Some("efiling_fee_receipt_once"))
            {
                return invalid("the fee receipt is already linked to an envelope");
            }
            error.into()
        })?
        .ok_or_else(|| invalid("the envelope already has a different fee observation"))
}

/// Facts from the exact card-transaction read, after the caller confirms the
/// match against the private envelope receipt.
pub struct SettledCharge {
    pub bank_account: Uuid,
    pub transaction: Uuid,
    pub amount_cents: i64,
    pub posted_on: NaiveDate,
}

struct FilingFee {
    id: Uuid,
    status: String,
    fee_receipt_document_id: Uuid,
    fee_amount_cents: i64,
    fee_currency: String,
    fee_cost_id: Option<Uuid>,
    fee_source_reference: Option<String>,
    fee_bank_account_id: Option<Uuid>,
}

impl FromRow<'_, PgRow> for FilingFee {
    fn from_row(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            status: row.try_get("status")?,
            fee_receipt_document_id: row.try_get("fee_receipt_document_id")?,
            fee_amount_cents: row.try_get("fee_amount_cents")?,
            fee_currency: row.try_get("fee_currency")?,
            fee_cost_id: row.try_get("fee_cost_id")?,
            fee_source_reference: row.try_get("fee_source_reference")?,
            fee_bank_account_id: row.try_get("fee_bank_account_id")?,
        })
    }
}

async fn find_fee<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    project_id: Uuid,
    envelope: &str,
) -> Result<FilingFee, EfilingError> {
    let filing: FilingFee = sqlx::query_as(
        "SELECT id, status, fee_receipt_document_id, fee_amount_cents, fee_currency,
                fee_cost_id, fee_source_reference, fee_bank_account_id
         FROM efiling_transactions
         WHERE project_id=$1 AND transaction_id=$2 AND fee_receipt_document_id IS NOT NULL",
    )
    .bind(project_id)
    .bind(envelope)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| invalid("record the private fee receipt on the accepted envelope first"))?;

    if filing.status != STATUS_ACCEPTED {
        return Err(invalid("a fee settlement requires an accepted envelope"));
    }
    Ok(filing)
}

impl FilingFee {
    fn existing_cost(&self, account: Uuid, source: &str) -> Result<Option<Uuid>, EfilingError> {
        if self.fee_cost_id.is_some()
            && (self.fee_source_reference.as_deref() != Some(source)
                || self.fee_bank_account_id != Some(account))
        {
            return Err(invalid(
                "the envelope already has a different settled charge",
            ));
        }
        Ok(self.fee_cost_id)
    }
}

/// Return the cost on an identical retry without another provider read.
/// A first attempt still requires a verified card purchase.
pub async fn existing_settlement(
    pg: &PostgresDb,
    project_id: Uuid,
    envelope: &str,
    account: Uuid,
    transaction: Uuid,
) -> Result<Option<Uuid>, EfilingError> {
    find_fee(pg.pool(), project_id, envelope)
        .await?
        .existing_cost(account, &costs::mercury_source_reference(transaction))
}

/// Post a settled charge exactly once, or link the identical cost already
/// imported for the same Project. The envelope link and cost commit together.
pub async fn settle(
    pg: &PostgresDb,
    project_id: Uuid,
    actor_id: Uuid,
    envelope: &str,
    charge: &SettledCharge,
) -> Result<Uuid, EfilingError> {
    let envelope = super::normalize_transaction_id(envelope).map_err(EfilingError::Invalid)?;
    let mut tx = pg.pool().begin().await?;
    costs::lock_project(&mut tx, project_id).await?;
    let filing = find_fee(&mut *tx, project_id, &envelope).await?;
    let source = costs::mercury_source_reference(charge.transaction);
    if filing.fee_currency != "USD"
        || charge.amount_cents <= 0
        || charge.amount_cents != filing.fee_amount_cents
    {
        return Err(invalid(
            "the settled card charge must match the accepted envelope fee in USD",
        ));
    }
    if let Some(id) = filing.existing_cost(charge.bank_account, &source)? {
        return Ok(id);
    }

    let receipt = receipt(
        &mut tx,
        project_id,
        filing.fee_receipt_document_id,
        &envelope,
    )
    .await?;
    if receipt.amount_cents != filing.fee_amount_cents || receipt.currency != filing.fee_currency {
        return Err(invalid(
            "the fee receipt no longer matches the envelope observation",
        ));
    }

    costs::lock_source(&mut tx, &source).await?;
    let claimed: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT FROM project_costs WHERE source_reference=$1 AND project_id<>$2)
              OR EXISTS(SELECT FROM efiling_transactions WHERE fee_source_reference=$1)",
    )
    .bind(&source)
    .bind(project_id)
    .fetch_one(&mut *tx)
    .await?;
    if claimed {
        return Err(invalid("the charge is already allocated"));
    }

    let input = costs::NewProjectCost {
        occurred_on: charge.posted_on,
        payee: receipt.payee,
        description: receipt.category.label().into(),
        amount_cents: charge.amount_cents,
        currency: receipt.currency,
        amount_status: "confirmed".into(),
        receipt_document_id: filing.fee_receipt_document_id,
        idempotency_key: format!("efiling-fee::{source}"),
        source_reference: source,
    };
    let (cost, _) = costs::create_on(&mut tx, project_id, actor_id, &input, true).await?;
    if cost.amount_status != "confirmed"
        || cost.amount_cents != input.amount_cents
        || cost.currency != input.currency
        || cost.occurred_on != charge.posted_on
    {
        return Err(invalid(
            "the existing Project Cost does not match the settled charge",
        ));
    }
    sqlx::query(
        "UPDATE efiling_transactions SET fee_cost_id=$1, fee_source_reference=$2,
           fee_bank_account_id=$3, fee_matched_by=$4, fee_matched_at=now(), updated_at=now()
         WHERE id=$5",
    )
    .bind(cost.id)
    .bind(input.source_reference)
    .bind(charge.bank_account)
    .bind(actor_id)
    .bind(filing.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(cost.id)
}

#[cfg(test)]
mod tests;
