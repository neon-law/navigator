//! The PostgreSQL `esignature_webhook_journal` table, the journal's system of
//! record whenever PostgreSQL is configured. The unique event index
//! `(provider, provider_id, status)` deduplicates a redelivery; a racing
//! insert loses that index and re-reads the winner's row.

use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use super::{ReceiptError, ReceiptRow, WebhookReceipt, SELECT, TABLE};
use crate::postgres::PostgresDb;
use crate::postgres_backfill::Copied;
use crate::signatures::SignatureProvider;
use crate::surreal::SurrealDb;

fn receipt(row: &PgRow) -> Result<WebhookReceipt, sqlx::Error> {
    Ok(WebhookReceipt {
        id: row.try_get("id")?,
        provider: row.try_get("provider")?,
        provider_id: row.try_get("provider_id")?,
        status: row.try_get("status")?,
        payload: row.try_get("payload")?,
        consumed_at: row.try_get("consumed_at")?,
        attempts: row.try_get("attempts")?,
        last_error: row.try_get("last_error")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// See [`super::by_event`].
pub(super) async fn by_event(
    pg: &PostgresDb,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
) -> Result<Option<WebhookReceipt>, ReceiptError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE provider = $1 AND provider_id = $2 AND status = $3"
    )))
    .bind(provider.as_str())
    .bind(provider_id)
    .bind(status)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(receipt)
    .transpose()?)
}

/// See [`super::journal_verified_receipt`].
pub(super) async fn journal_verified_receipt(
    pg: &PostgresDb,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
    payload: &str,
) -> Result<WebhookReceipt, ReceiptError> {
    let inserted = sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, provider, provider_id, status, payload, attempts, \
         inserted_at, updated_at) VALUES ($1, $2, $3, $4, $5, 1, now(), now()) \
         ON CONFLICT (provider, provider_id, status) DO NOTHING RETURNING {SELECT}"
    )))
    .bind(Uuid::now_v7())
    .bind(provider.as_str())
    .bind(provider_id)
    .bind(status)
    .bind(payload)
    .fetch_optional(&pg.0)
    .await?;
    match inserted {
        Some(row) => Ok(receipt(&row)?),
        None => by_event(pg, provider, provider_id, status)
            .await?
            .ok_or(ReceiptError::WriteReturnedNothing),
    }
}

/// See [`super::pending_receipts`].
pub(super) async fn pending_receipts(
    pg: &PostgresDb,
    limit: i64,
) -> Result<Vec<WebhookReceipt>, ReceiptError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE consumed_at IS NULL \
         ORDER BY inserted_at, id LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(receipt)
    .collect::<Result<_, _>>()?)
}

/// See [`super::mark_consumed`].
pub(super) async fn mark_consumed(pg: &PostgresDb, receipt_id: Uuid) -> Result<(), ReceiptError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {TABLE} SET consumed_at = now(), updated_at = now(), last_error = NULL \
         WHERE id = $1"
    )))
    .bind(receipt_id)
    .execute(&pg.0)
    .await?;
    Ok(())
}

/// See [`super::record_attempt_failure`].
pub(super) async fn record_attempt_failure(
    pg: &PostgresDb,
    receipt_id: Uuid,
    error: &str,
) -> Result<(), ReceiptError> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {TABLE} SET attempts = attempts + 1, last_error = $2, updated_at = now() \
         WHERE id = $1"
    )))
    .bind(receipt_id)
    .bind(error)
    .execute(&pg.0)
    .await?;
    Ok(())
}

/// Copy every SurrealDB receipt PostgreSQL does not hold, keeping its id. A
/// receipt PostgreSQL already journaled for the same event is left as is.
pub(crate) async fn copy_from_surreal(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Copied, surrealdb::Error> {
    let rows: Vec<ReceiptRow> = surreal
        .query(format!("SELECT {SELECT} FROM {TABLE}"))
        .await
        .and_then(surrealdb::IndexedResults::check)?
        .take(0)?;
    let mut copied = Copied::default();
    for row in rows {
        let Some(receipt) = row.into_receipt() else {
            copied.unrepresentable(TABLE);
            continue;
        };
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {TABLE} (id, provider, provider_id, status, payload, consumed_at, \
             attempts, last_error, inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) ON CONFLICT DO NOTHING"
        )))
        .bind(receipt.id)
        .bind(&receipt.provider)
        .bind(&receipt.provider_id)
        .bind(&receipt.status)
        .bind(&receipt.payload)
        .bind(receipt.consumed_at)
        .bind(receipt.attempts)
        .bind(&receipt.last_error)
        .bind(receipt.inserted_at)
        .bind(receipt.updated_at)
        .execute(&pg.0)
        .await
        .map(|done| done.rows_affected());
        copied.record(TABLE, Some(receipt.id), result);
    }
    Ok(copied)
}
