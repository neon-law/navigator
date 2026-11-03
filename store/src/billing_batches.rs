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
    #[error("the matter's flat fee has already been billed")]
    FlatFeeAlreadyBilled,
    #[error("billing batch was not found")]
    NotFound,
    #[error("time entry store failed: {0}")]
    Time(#[from] crate::time_entries::TimeEntryError),
    #[error("project cost store failed: {0}")]
    Cost(#[from] crate::project_costs::ProjectCostError),
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
    /// Frozen debtor details keep retries independent of later participation edits.
    pub contact_name: String,
    pub contact_email: String,
    /// Resolved once before reservation; retries keep this exact debtor identity.
    pub xero_contact_id: String,
    /// The first create attempt under this immutable batch idempotency key.
    pub first_attempted_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The frozen composed request: the exact line items the provider call
    /// carries, so a resume presents the same request under the same key.
    pub line_items: serde_json::Value,
    pub total_cents: i64,
    pub currency: String,
    pub entry_ids: Vec<Uuid>,
    pub cost_ids: Vec<Uuid>,
    /// The provider's invoice id, saved the moment a raise returns — a
    /// resume reuses it instead of re-raising under the provider's own
    /// (finite) idempotency window.
    pub xero_invoice_id: Option<String>,
    /// The portion of the matter's agreed flat fee charged by this batch.
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
        contact_name: row.try_get("contact_name")?,
        contact_email: row.try_get("contact_email")?,
        xero_contact_id: row.try_get("xero_contact_id")?,
        first_attempted_at: row.try_get("first_attempted_at")?,
        line_items: row.try_get("line_items")?,
        total_cents: row.try_get("total_cents")?,
        currency: row.try_get("currency")?,
        entry_ids: row.try_get("entry_ids")?,
        cost_ids: row.try_get("cost_ids")?,
        xero_invoice_id: row.try_get("xero_invoice_id")?,
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
    let mut tx = pg.0.begin().await?;
    let batch = insert_in_tx(&mut tx, batch).await?;
    tx.commit().await?;
    Ok(batch)
}

async fn insert_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    batch: &MatterBillingBatch,
) -> Result<MatterBillingBatch, BillingBatchError> {
    let row = sqlx::query(
        "INSERT INTO matter_billing_batch(id,project_id,status,reference,line_items,total_cents,currency,entry_ids,cost_ids,xero_invoice_id,flat_fee_cents,actor_id,inserted_at,updated_at,contact_name,contact_email,first_attempted_at,xero_contact_id) \
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18) RETURNING *",
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
    .bind(&batch.xero_invoice_id)
    .bind(batch.flat_fee_cents)
    .bind(batch.actor_id)
    .bind(batch.inserted_at)
    .bind(batch.updated_at)
    .bind(&batch.contact_name)
    .bind(&batch.contact_email)
    .bind(batch.first_attempted_at)
    .bind(&batch.xero_contact_id)
    .fetch_one(&mut **tx)
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

/// Record the first create attempt once before crossing the provider boundary.
/// Concurrent callers keep the same timestamp and immutable batch key; a
/// saved invoice bypasses a new create attempt entirely.
pub async fn start_attempt(
    pg: &PostgresDb,
    id: Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<MatterBillingBatch, BillingBatchError> {
    let row = sqlx::query(
        "UPDATE matter_billing_batch SET \
         first_attempted_at=CASE WHEN xero_invoice_id IS NULL THEN COALESCE(first_attempted_at,$2) ELSE first_attempted_at END, \
         updated_at=CASE WHEN xero_invoice_id IS NULL AND first_attempted_at IS NULL THEN $2 ELSE updated_at END \
         WHERE id=$1 RETURNING *",
    )
    .bind(id).bind(now).fetch_optional(&pg.0).await?
    .ok_or(BillingBatchError::NotFound)?;
    from_row(&row).map_err(BillingBatchError::Database)
}

/// Save the provider's invoice id the moment the raise returns — before any
/// local flip — so a resumed batch reuses the exact invoice instead of
/// re-raising and relying on the provider's own idempotency window.
pub async fn attach_invoice(
    pg: &PostgresDb,
    id: Uuid,
    xero_invoice_id: &str,
) -> Result<MatterBillingBatch, BillingBatchError> {
    let row = sqlx::query(
        "UPDATE matter_billing_batch SET xero_invoice_id=$2, \
         updated_at=CASE WHEN xero_invoice_id IS NULL THEN $3 ELSE updated_at END \
         WHERE id=$1 AND (xero_invoice_id IS NULL OR xero_invoice_id=$2) RETURNING *",
    )
    .bind(id)
    .bind(xero_invoice_id)
    .bind(chrono::Utc::now())
    .fetch_optional(&pg.0)
    .await?
    .ok_or(BillingBatchError::NotFound)?;
    from_row(&row).map_err(BillingBatchError::Database)
}

/// Reserve the batch's rows and persist its intent in ONE transaction — the
/// other order (reserve, then insert) can crash in between and strand
/// reserved work with no batch to resume it.
pub async fn insert_with_reservations(
    pg: &PostgresDb,
    batch: &MatterBillingBatch,
    expected_billed_fee_cents: Option<i64>,
) -> Result<MatterBillingBatch, BillingBatchError> {
    if batch.total_cents <= 0 {
        return Err(BillingBatchError::Invalid("total"));
    }
    if batch.status != "pending" {
        return Err(BillingBatchError::Invalid("status"));
    }
    let mut tx = pg.0.begin().await?;
    // Serialize even fee-only batches, which have no work rows to lock.
    // Recheck under this lock: an earlier request may have completed since
    // the caller composed its invoice.
    sqlx::query("SELECT id FROM project WHERE id=$1 FOR UPDATE")
        .bind(batch.project_id)
        .fetch_one(&mut *tx)
        .await?;
    if let Some(expected) = expected_billed_fee_cents {
        let billed: i64 = sqlx::query_scalar(BILLED_FLAT_FEE_SQL)
            .bind(batch.project_id)
            .fetch_one(&mut *tx)
            .await?;
        if billed != expected {
            return Err(BillingBatchError::FlatFeeAlreadyBilled);
        }
    }
    if !batch.entry_ids.is_empty() {
        crate::time_entries::reserve_in_tx(&mut tx, &batch.entry_ids, batch.actor_id)
            .await
            .map_err(BillingBatchError::from)?;
    }
    if !batch.cost_ids.is_empty() {
        crate::project_costs::reserve_in_tx(&mut tx, &batch.cost_ids, batch.actor_id)
            .await
            .map_err(BillingBatchError::from)?;
    }
    let batch = insert_in_tx(&mut tx, batch).await?;
    tx.commit().await?;
    Ok(batch)
}

const BILLED_FLAT_FEE_SQL: &str =
    "SELECT COALESCE(SUM(flat_fee_cents),0)::bigint FROM matter_billing_batch \
     WHERE project_id=$1 AND status='completed'";

/// The portions of the agreed flat fee already charged by completed batches.
/// An amended fee bills only the difference from this total.
pub async fn flat_fee_billed_cents(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<i64, BillingBatchError> {
    Ok(sqlx::query_scalar(BILLED_FLAT_FEE_SQL)
        .bind(project_id)
        .fetch_one(&pg.0)
        .await?)
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
    let mut tx = pg.0.begin().await?;
    // Completion releases the pending unique index, so it participates in
    // the same matter lock as insertion. Otherwise a stale fee composition
    // could recheck before completion and insert just after it.
    sqlx::query(
        "SELECT project.id FROM project JOIN matter_billing_batch batch \
         ON batch.project_id=project.id WHERE batch.id=$1 FOR UPDATE OF project",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BillingBatchError::NotFound)?;
    let row = sqlx::query(
        "UPDATE matter_billing_batch SET status='completed',updated_at=$2 \
         WHERE id=$1 RETURNING *",
    )
    .bind(id)
    .bind(chrono::Utc::now())
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BillingBatchError::NotFound)?;
    tx.commit().await?;
    from_row(&row).map_err(BillingBatchError::Database)
}
