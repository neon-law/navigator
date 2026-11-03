//! Matter billing batches — one persisted invoice intent per batch.
//!
//! A batch freezes the composed request (line items, reference, the row ids
//! it consumes) BEFORE the provider is called. The batch's own id is the
//! idempotency key passed to [`crate::billing`]'s `create_invoice`, so a
//! retry of the same generation re-mints the same invoice, and a later
//! billing batch — new work after an earlier invoice — composes a new one
//! instead of silently re-minting the first. While a batch is pending, its
//! rows are reserved: write-offs and cost resolution refuse them, so billed
//! work cannot be removed behind the provider's back.
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum BillingBatchError {
    #[error("invalid billing batch: {0}")]
    Invalid(&'static str),
    #[error("a pending batch already exists for this matter")]
    PendingExists,
    #[error("billing batch was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterBillingBatch {
    pub id: Uuid,
    pub project_id: Uuid,
    pub status: String,
    /// The human-facing matter reference shown on the invoice.
    pub reference: String,
    /// The frozen composed request: the exact line items the provider call
    /// carries, so a resume presents the same request under the same key.
    pub line_items: serde_json::Value,
    pub total_cents: i64,
    pub currency: String,
    pub entry_ids: Vec<Uuid>,
    pub cost_ids: Vec<Uuid>,
    /// The flat fee the batch bills, when the matter carries one.
    pub flat_fee_cents: Option<i64>,
    pub actor_id: Uuid,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn from_row(row: &sqlx::postgres::PgRow) -> Result<MatterBillingBatch, sqlx::Error> {
    Ok(MatterBillingBatch {
        id: row.try_get("id")?,
        project_id: row.try_get("project_id")?,
        status: row.try_get("status")?,
        reference: row.try_get("reference")?,
        line_items: row.try_get("line_items")?,
        total_cents: row.try_get("total_cents")?,
        currency: row.try_get("currency")?,
        entry_ids: row.try_get("entry_ids")?,
        cost_ids: row.try_get("cost_ids")?,
        flat_fee_cents: row.try_get("flat_fee_cents")?,
        actor_id: row.try_get("actor_id")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Persist a new pending batch. The caller has composed the request and
/// reserved its rows; the batch row makes the intent durable before the
/// provider call.
pub async fn insert(
    pg: &PostgresDb,
    batch: &MatterBillingBatch,
) -> Result<MatterBillingBatch, BillingBatchError> {
    if batch.total_cents <= 0 {
        return Err(BillingBatchError::Invalid("total"));
    }
    if batch.status != "pending" {
        return Err(BillingBatchError::Invalid("status"));
    }
    let row = sqlx::query(
        "INSERT INTO matter_billing_batch(id,project_id,status,reference,line_items,total_cents,currency,entry_ids,cost_ids,flat_fee_cents,actor_id,inserted_at,updated_at) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING *",
    )
    .bind(batch.id)
    .bind(batch.project_id)
    .bind(&batch.status)
    .bind(&batch.reference)
    .bind(&batch.line_items)
    .bind(batch.total_cents)
    .bind(&batch.currency)
    .bind(&batch.entry_ids)
    .bind(&batch.cost_ids)
    .bind(batch.flat_fee_cents)
    .bind(batch.actor_id)
    .bind(batch.inserted_at)
    .bind(batch.updated_at)
    .fetch_one(&pg.0)
    .await
    .map_err(|error| {
        // The partial unique index enforces one pending batch per matter.
        if error.as_database_error().is_some_and(sqlx::error::DatabaseError::is_unique_violation) {
            BillingBatchError::PendingExists
        } else {
            BillingBatchError::Database(error)
        }
    })?;
    from_row(&row).map_err(BillingBatchError::Database)
}

/// The matter's open invoice intent, if any — the batch a retry resumes.
pub async fn pending_for_project(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<MatterBillingBatch>, BillingBatchError> {
    let row = sqlx::query(
        "SELECT * FROM matter_billing_batch WHERE project_id=$1 AND status='pending' \
         ORDER BY inserted_at DESC, id LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(&pg.0)
    .await?;
    Ok(row.as_ref().map(from_row).transpose()?)
}

/// Complete a batch: its invoice exists in Xero, its mirror row is written,
/// and its rows are invoiced. The next generation composes a new batch.
pub async fn complete(pg: &PostgresDb, id: Uuid) -> Result<MatterBillingBatch, BillingBatchError> {
    let row = sqlx::query(
        "UPDATE matter_billing_batch SET status='completed',updated_at=$2 \
         WHERE id=$1 AND status='pending' RETURNING *",
    )
    .bind(id)
    .bind(chrono::Utc::now())
    .fetch_optional(&pg.0)
    .await?
    .ok_or(BillingBatchError::NotFound)?;
    from_row(&row).map_err(BillingBatchError::Database)
}
