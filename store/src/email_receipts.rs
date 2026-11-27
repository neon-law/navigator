//! Persistent idempotency records for authenticated inbound summary mail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::communications::{self as authority, CommunicationsAuthorityError};
use crate::postgres::PostgresDb;

pub mod summary_recovery;

pub const TABLE: &str = "email_receipt";
pub const PROCESSING_PENDING: &str = "pending";
pub const PROCESSING_ARCHIVED: &str = "archived";
pub const DELIVERY_NOT_ATTEMPTED: &str = "not_attempted";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailReceipt {
    pub id: Uuid,
    pub receiving_mailbox: String,
    pub deployment: String,
    /// Firm and provider account that admitted this summary receipt. Ordinary
    /// outbound delivery receipts leave these absent and cannot pass the
    /// summary sender gate.
    pub firm_id: Option<Uuid>,
    pub provider_account: Option<String>,
    /// True only when the inbound provider authenticated the From domain.
    /// This is separate from a Person's confirmed mailbox and explicit approval.
    pub sender_authenticated: bool,
    pub raw_digest: String,
    pub source_message_id: Option<String>,
    pub archive_key: String,
    pub letter_id: Uuid,
    pub processing_state: String,
    pub delivery_state: String,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct NewEmailReceipt<'a> {
    pub receiving_mailbox: &'a str,
    pub deployment: &'a str,
    pub firm_id: Option<Uuid>,
    pub provider_account: Option<&'a str>,
    pub sender_authenticated: bool,
    pub raw_digest: &'a str,
    pub source_message_id: Option<&'a str>,
    pub archive_key: &'a str,
    pub letter_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ensured {
    pub receipt: EmailReceipt,
    pub deduped: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EmailReceiptError {
    #[error("delivery lookup: {0}")]
    Delivery(#[from] crate::email_deliveries::EmailDeliveryError),
    #[error(transparent)]
    Postgres(#[from] CommunicationsAuthorityError),
    #[error("writing an email receipt returned no usable row")]
    WriteReturnedNothing,
}

/// A PostgreSQL receipt as `row`, its state taken from its delivery, which
/// is authoritative over the receipt's own column (see `with_delivery_state`).
macro_rules! pg_receipt {
    () => {
        "SELECT to_jsonb(r) || jsonb_build_object('delivery_state', \
         COALESCE(d.state, r.delivery_state)) AS row \
         FROM email_receipt r LEFT JOIN email_delivery d ON d.receipt_id = r.id"
    };
}

pub async fn ensure(
    _db: &ApplicationStore,
    new: &NewEmailReceipt<'_>,
) -> Result<Ensured, EmailReceiptError> {
    let pg = authority::authority().await?;
    Box::pin(ensure_pg(&pg, new)).await
}

pub async fn find(
    _db: &ApplicationStore,
    receiving_mailbox: &str,
    deployment: &str,
    raw_digest: &str,
) -> Result<Option<EmailReceipt>, EmailReceiptError> {
    let pg = authority::authority().await?;
    Ok(find_pg(&pg, receiving_mailbox, deployment, raw_digest).await?)
}

/// Find a receipt by its opaque durable-workflow key.
pub async fn find_by_id(
    _db: &ApplicationStore,
    id: Uuid,
) -> Result<Option<EmailReceipt>, EmailReceiptError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(concat!(pg_receipt!(), " WHERE r.id = $1")).bind(id);
    Ok(authority::maybe(&pg, query).await?)
}

// The delivery record is authoritative, including for receipts stored before
// a delivery existed. Reading it avoids a second writable copy of the status.

pub async fn mark_archived(_db: &ApplicationStore, id: Uuid) -> Result<(), EmailReceiptError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE email_receipt SET processing_state = $2, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(PROCESSING_ARCHIVED);
    authority::execute(&pg, query).await?;
    Ok(())
}

/// Receipts past a caller-supplied deadline without authoritative confirmation.
/// Includes partial archives and missed triggers; contains no message content.
pub async fn list_overdue(
    _db: &ApplicationStore,
    before: DateTime<Utc>,
) -> Result<Vec<EmailReceipt>, EmailReceiptError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(r) AS row FROM email_receipt r WHERE r.inserted_at < $1 \
         AND NOT EXISTS (SELECT 1 FROM email_delivery d WHERE d.receipt_id = r.id \
         AND d.state = 'confirmed') ORDER BY r.inserted_at",
    )
    .bind(before);
    Ok(authority::rows(&pg, query).await?)
}

pub async fn count(_db: &ApplicationStore) -> Result<i64, EmailReceiptError> {
    let pg = authority::authority().await?;
    let query = sqlx::query("SELECT to_jsonb(count(*)) AS row FROM email_receipt");
    Ok(authority::maybe(&pg, query).await?.unwrap_or(0))
}

fn find_pg<'a>(
    pg: &'a PostgresDb,
    receiving_mailbox: &str,
    deployment: &str,
    raw_digest: &str,
) -> authority::Op<'a, Option<EmailReceipt>> {
    let query = sqlx::query(concat!(
        pg_receipt!(),
        " WHERE r.receiving_mailbox = $1 AND r.deployment = $2 AND r.raw_digest = $3"
    ))
    .bind(receiving_mailbox.to_owned())
    .bind(deployment.to_owned())
    .bind(raw_digest.to_owned());
    authority::maybe(pg, query)
}

/// The unique `email_receipt_identity` index decides a race: the loser's
/// insert does nothing and it reads the winner's row.
async fn ensure_pg(
    pg: &PostgresDb,
    new: &NewEmailReceipt<'_>,
) -> Result<Ensured, EmailReceiptError> {
    let query = sqlx::query(
        "INSERT INTO email_receipt (id, receiving_mailbox, deployment, firm_id, \
         provider_account, sender_authenticated, raw_digest, source_message_id, archive_key, \
         letter_id, processing_state, delivery_state, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now(), now()) \
         ON CONFLICT (receiving_mailbox, deployment, raw_digest) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(new.receiving_mailbox.to_owned())
    .bind(new.deployment.to_owned())
    .bind(new.firm_id)
    .bind(new.provider_account.map(str::to_owned))
    .bind(new.sender_authenticated)
    .bind(new.raw_digest.to_owned())
    .bind(new.source_message_id.map(str::to_owned))
    .bind(new.archive_key.to_owned())
    .bind(new.letter_id)
    .bind(PROCESSING_PENDING)
    .bind(DELIVERY_NOT_ATTEMPTED);
    let inserted = authority::execute(pg, query).await?;
    let receipt = find_pg(pg, new.receiving_mailbox, new.deployment, new.raw_digest)
        .await?
        .ok_or(EmailReceiptError::WriteReturnedNothing)?;
    Ok(Ensured {
        receipt,
        deduped: inserted == 0,
    })
}

#[cfg(test)]
mod tests;
