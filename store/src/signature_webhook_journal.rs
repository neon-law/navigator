//! `esignature_webhook_journal` — the durable archive of verified
//! e-signature webhook receipts, and the reads the boot-time replay sweep
//! drains by.
//!
//! # Journal first, consume second
//!
//! The inbound completion webhook (`portal::esignature_webhook`) advances
//! workflow state from the public internet. Advancing used to be the first
//! durable write, and every follow-on failure was best-effort: a worker
//! killed mid-delivery lost the event, and the provider's redelivery was
//! the only recovery — which stops the first time the endpoint answers
//! `200`. The journal flips the order. A verified, parseable receipt is
//! written **before** any consumption; a consumption failure leaves the
//! row pending (`consumed_at` `NONE`) with the failure recorded; the
//! boot-time sweep re-runs pending receipts through the same consumption
//! function the handler used. A delivery is recovered from the journal,
//! not lost — delivery is replayable instead of best-effort.
//!
//! Only *verified* payloads are journaled. HMAC verification refuses an
//! unverified delivery before any write, and a refusal is not an event —
//! this archive is the record of what the trusted provider said, not of
//! everything that hit the endpoint.
//!
//! Redelivery of the same event lands on the same row: `(provider,
//! provider_id, status)` is unique, and a racing insert loses the index
//! and re-reads the winner's row — the same idempotent-write pattern
//! [`crate::signatures`] uses for `(provider, provider_id)`.

use chrono::{DateTime, Utc};
use serde::Serialize;
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::signatures::SignatureProvider;
use crate::surreal::{record_id, record_uuid, retry, SurrealDb};

/// The table these rows live in.
pub(crate) const TABLE: &str = "esignature_webhook_journal";

/// The receipt's own unique index: one row per provider event. The
/// provider's redelivery of the same event resolves here instead of
/// double-journaling, and the replay sweep never sees a duplicate.
pub(crate) const EVENT_INDEX: &str = "esignature_webhook_journal_event";

/// One verified e-signature webhook receipt: what the provider said, and
/// whether it has been consumed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebhookReceipt {
    pub id: Uuid,
    /// The provider's stored string — see [`SignatureProvider::as_str`].
    pub provider: String,
    /// The provider's opaque envelope id, correlated back to
    /// `signature` by the same `(provider, provider_id)` pair.
    pub provider_id: String,
    /// The envelope status exactly as the payload carried it. The journal
    /// is an archive, not a filter: a status the handler does not classify
    /// is still stored — replay re-runs the payload's bytes, and the
    /// consumer re-derives what to do from them.
    pub status: String,
    /// The verified payload as its exact JSON text, so replay re-runs the
    /// bytes rather than an interpretation of them.
    pub payload: String,
    /// `None` until consumption (workflow advance + signature stamp +
    /// document archival) succeeds. The replay sweep drains `None`s
    /// oldest-first.
    pub consumed_at: Option<DateTime<Utc>>,
    /// How many consumption attempts this row has seen, including the
    /// original delivery. Observability for the sweep, not a backoff
    /// input.
    pub attempts: i64,
    /// The last consumption failure, if any — the replay report's first
    /// line.
    pub last_error: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The row as the engine reads and writes it.
#[derive(SurrealValue)]
struct ReceiptRow {
    id: surrealdb::types::RecordId,
    provider: String,
    provider_id: String,
    status: String,
    payload: String,
    consumed_at: Option<surrealdb::types::Datetime>,
    attempts: Option<i64>,
    last_error: Option<String>,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

impl ReceiptRow {
    /// `None` when a record id is not a native UUID key — a row written by
    /// something that bypassed [`crate::surreal::record_id`].
    fn into_receipt(self) -> Option<WebhookReceipt> {
        Some(WebhookReceipt {
            id: record_uuid(&self.id)?,
            provider: self.provider,
            provider_id: self.provider_id,
            status: self.status,
            payload: self.payload,
            consumed_at: self.consumed_at.map(Into::into),
            attempts: self.attempts.unwrap_or(0),
            last_error: self.last_error,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`ReceiptRow`] from only one query.
const SELECT: &str = "id, provider, provider_id, status, payload, consumed_at, attempts, \
     last_error, inserted_at, updated_at";

/// Errors reading or writing a webhook receipt.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a webhook receipt returned no usable row")]
    WriteReturnedNothing,
}

/// `True` when the engine refused the insert on the receipt's event index
/// — the provider redelivered an event that is already journaled.
fn is_event_conflict(error: &surrealdb::Error) -> bool {
    retry::unique_violation(error) == Some(EVENT_INDEX)
}

/// Run a write under the shared retry policy ([`crate::surreal::retry`]),
/// mapping whatever finally comes back to this module's error. Only the
/// mapping lives here; the retry budget is one policy for the whole crate.
async fn writing<F, Q>(attempt: F) -> Result<surrealdb::IndexedResults, surrealdb::Error>
where
    F: FnMut() -> Q,
    Q: std::future::IntoFuture<Output = Result<surrealdb::IndexedResults, surrealdb::Error>>,
{
    retry::writing(attempt).await
}

fn one(
    mut response: surrealdb::IndexedResults,
) -> Result<Option<WebhookReceipt>, surrealdb::Error> {
    let row: Option<ReceiptRow> = response.take(0)?;
    Ok(row.and_then(ReceiptRow::into_receipt))
}

/// The receipt for one provider event, if any — the journal's canonical
/// event lookup. The unique index enforces at most one row per key
/// (provider, envelope, status), so a caller holding a returned receipt
/// knows a redelivery of the same event resolves to this same row.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the lookup fails.
pub async fn by_event(
    db: &SurrealDb,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
) -> Result<Option<WebhookReceipt>, ReceiptError> {
    let response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE provider = $provider AND provider_id = $provider_id \
             AND status = $status LIMIT 1"
        ))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("provider_id", provider_id.to_string()))
        .bind(("status", status.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    Ok(one(response)?)
}

/// Journal a verified, parseable webhook receipt **before** it is
/// consumed. Idempotent on the event: a redelivery returns the existing
/// row rather than inserting a duplicate — a concurrent double-delivery
/// loses the event index and re-reads the winner's row.
///
/// The caller has already verified the HMAC over `payload`'s exact bytes;
/// nothing unverified reaches this table.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the write fails,
/// [`ReceiptError::WriteReturnedNothing`] if the write cannot be read
/// back.
pub async fn journal_verified_receipt(
    db: &SurrealDb,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
    payload: &str,
) -> Result<WebhookReceipt, ReceiptError> {
    let id = Uuid::now_v7();
    match writing(|| {
        db.query(format!(
            "CREATE $id SET \
             provider = $provider, provider_id = $provider_id, status = $status, \
             payload = $payload, attempts = 1 \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("provider", provider.as_str().to_string()))
        .bind(("provider_id", provider_id.to_string()))
        .bind(("status", status.to_string()))
        .bind(("payload", payload.to_string()))
    })
    .await
    {
        Ok(mut response) => {
            let row: Option<ReceiptRow> = response.take(0)?;
            row.and_then(ReceiptRow::into_receipt)
                .ok_or(ReceiptError::WriteReturnedNothing)
        }
        Err(error) if is_event_conflict(&error) => by_event(db, provider, provider_id, status)
            .await?
            .ok_or(ReceiptError::WriteReturnedNothing),
        Err(error) => Err(ReceiptError::Db(error)),
    }
}

/// The journal's pending receipts, oldest first — the replay sweep's
/// work list. `limit` bounds one sweep's work so a restart with a large
/// backlog converges over several boots instead of holding the boot host.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the read fails.
pub async fn pending_receipts(
    db: &SurrealDb,
    limit: i64,
) -> Result<Vec<WebhookReceipt>, ReceiptError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE consumed_at IS NONE \
             ORDER BY inserted_at ASC LIMIT $limit"
        ))
        .bind(("limit", limit))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<ReceiptRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(ReceiptRow::into_receipt)
        .collect())
}

/// Mark a receipt consumed: the workflow advanced, the signature row is
/// stamped, and the executed documents are archived. A redelivery of the
/// same event after this point is answered from the journal, not
/// re-consumed.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the write fails.
pub async fn mark_consumed(db: &SurrealDb, receipt_id: Uuid) -> Result<(), ReceiptError> {
    writing(|| {
        db.query(format!(
            "UPDATE $id SET consumed_at = time::now(), updated_at = time::now(), \
             last_error = NONE RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, receipt_id)))
    })
    .await
    .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

/// Record a failed consumption attempt on a pending receipt: the attempt
/// count rises, the failure is named, and the row stays pending for the
/// next sweep. A recovery that silently dropped the error would be a
/// journal that lies.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the write fails.
pub async fn record_attempt_failure(
    db: &SurrealDb,
    receipt_id: Uuid,
    error: &str,
) -> Result<(), ReceiptError> {
    writing(|| {
        db.query(format!(
            "UPDATE $id SET attempts += 1, last_error = $error, updated_at = time::now() \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, receipt_id)))
        .bind(("error", error.to_string()))
    })
    .await
    .and_then(surrealdb::IndexedResults::check)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_redelivery_conflict_names_the_receipt_event_index() {
        let error = surrealdb::Error::internal(format!(
            "Database index `{EVENT_INDEX}` already contains this value"
        ));

        assert!(is_event_conflict(&error));
    }

    #[test]
    fn a_conflict_on_another_index_is_not_a_redelivery() {
        let error = surrealdb::Error::internal(
            "Database index `person_email_lower` already contains this value".to_string(),
        );

        assert!(!is_event_conflict(&error));
    }

    #[test]
    fn an_unrelated_write_failure_is_not_a_redelivery() {
        let error = surrealdb::Error::internal("connection closed".to_string());

        assert!(!is_event_conflict(&error));
    }

    #[tokio::test]
    async fn a_consumed_receipt_stops_being_pending() {
        let db = crate::surreal::test_support::mem().await;
        let receipt = journal_verified_receipt(
            &db,
            SignatureProvider::DocuSign,
            "fake-envelope-1",
            "completed",
            "{\"event\":\"envelope-completed\"}",
        )
        .await
        .expect("journal the receipt");

        let pending = pending_receipts(&db, 10).await.expect("read the journal");
        assert_eq!(pending.len(), 1, "a fresh receipt is pending");
        assert_eq!(
            pending[0].id, receipt.id,
            "the pending row hands back the journal's id"
        );

        // Mark through the row the sweep fetched — the same id
        // `pending_receipts` returns — as `replay_one` does.
        mark_consumed(&db, pending[0].id)
            .await
            .expect("mark the receipt consumed");

        let pending = pending_receipts(&db, 10).await.expect("read the journal");
        assert!(pending.is_empty(), "a consumed receipt stops being pending");
    }
}
