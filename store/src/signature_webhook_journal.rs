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
//!
//! PostgreSQL holds the journal whenever `NAVIGATOR_POSTGRES_HOST` is set
//! ([`PostgresDb::configured`]); without a PostgreSQL host the same calls use
//! the SurrealDB table. Receipts SurrealDB still holds reach PostgreSQL
//! through the Mirror Backfill's born-in-PostgreSQL arm.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::signatures::SignatureProvider;

pub(crate) mod postgres;
#[cfg(test)]
mod postgres_tests;

/// The table these rows live in.
pub(crate) const TABLE: &str = "esignature_webhook_journal";

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

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`ReceiptRow`] from only one query.
const SELECT: &str = "id, provider, provider_id, status, payload, consumed_at, attempts, \
     last_error, inserted_at, updated_at";

/// Errors reading or writing a webhook receipt.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptError {
    /// The configured PostgreSQL server could not be reached.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] crate::postgres::PostgresError),
    /// A PostgreSQL statement failed.
    #[error("PostgreSQL query: {0}")]
    Sql(#[from] sqlx::Error),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a webhook receipt returned no usable row")]
    WriteReturnedNothing,
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
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
) -> Result<Option<WebhookReceipt>, ReceiptError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::by_event(&pg, provider, provider_id, status)).await
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
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    status: &str,
    payload: &str,
) -> Result<WebhookReceipt, ReceiptError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::journal_verified_receipt(
        &pg,
        provider,
        provider_id,
        status,
        payload,
    ))
    .await
}

/// The journal's pending receipts, oldest first — the replay sweep's
/// work list. `limit` bounds one sweep's work so a restart with a large
/// backlog converges over several boots instead of holding the boot host.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the read fails.
pub async fn pending_receipts(
    _db: &ApplicationStore,
    limit: i64,
) -> Result<Vec<WebhookReceipt>, ReceiptError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::pending_receipts(&pg, limit)).await
}

/// Mark a receipt consumed: the workflow advanced, the signature row is
/// stamped, and the executed documents are archived. A redelivery of the
/// same event after this point is answered from the journal, not
/// re-consumed.
///
/// # Errors
///
/// [`ReceiptError::Db`] if the write fails.
pub async fn mark_consumed(_db: &ApplicationStore, receipt_id: Uuid) -> Result<(), ReceiptError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::mark_consumed(&pg, receipt_id)).await
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
    _db: &ApplicationStore,
    receipt_id: Uuid,
    error: &str,
) -> Result<(), ReceiptError> {
    let pg = PostgresDb::application().await?;
    Box::pin(postgres::record_attempt_failure(&pg, receipt_id, error)).await
}

#[cfg(test)]
mod tests {}
