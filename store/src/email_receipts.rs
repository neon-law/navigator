//! Persistent idempotency records for authenticated inbound summary mail.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::surreal::{record_id, record_uuid, SurrealDb};

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
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("writing an email receipt returned no usable row")]
    WriteReturnedNothing,
}

#[derive(SurrealValue)]
struct EmailReceiptRow {
    id: surrealdb::types::RecordId,
    receiving_mailbox: String,
    deployment: String,
    firm_id: Option<surrealdb::types::RecordId>,
    provider_account: Option<String>,
    sender_authenticated: Option<bool>,
    raw_digest: String,
    source_message_id: Option<String>,
    archive_key: String,
    letter_id: surrealdb::types::RecordId,
    processing_state: String,
    delivery_state: String,
    inserted_at: surrealdb::types::Datetime,
    updated_at: surrealdb::types::Datetime,
}

#[derive(SurrealValue)]
struct CountRow {
    count: i64,
}

const SELECT: &str = "id, receiving_mailbox, deployment, firm_id, provider_account, sender_authenticated, raw_digest, source_message_id, \
                      archive_key, letter_id, processing_state, delivery_state, inserted_at, updated_at";

impl EmailReceiptRow {
    fn into_receipt(self) -> Option<EmailReceipt> {
        Some(EmailReceipt {
            id: record_uuid(&self.id)?,
            receiving_mailbox: self.receiving_mailbox,
            deployment: self.deployment,
            firm_id: self.firm_id.as_ref().and_then(record_uuid),
            provider_account: self.provider_account,
            sender_authenticated: self.sender_authenticated.unwrap_or(false),
            raw_digest: self.raw_digest,
            source_message_id: self.source_message_id,
            archive_key: self.archive_key,
            letter_id: record_uuid(&self.letter_id)?,
            processing_state: self.processing_state,
            delivery_state: self.delivery_state,
            inserted_at: self.inserted_at.into(),
            updated_at: self.updated_at.into(),
        })
    }
}

pub async fn ensure(
    db: &SurrealDb,
    new: &NewEmailReceipt<'_>,
) -> Result<Ensured, EmailReceiptError> {
    if let Some(receipt) = find(db, new.receiving_mailbox, new.deployment, new.raw_digest).await? {
        crate::postgres_mirror::after_write(
            db,
            crate::postgres_mirror::MirrorKey::EmailReceipt(receipt.id),
        )
        .await;
        return Ok(Ensured {
            receipt,
            deduped: true,
        });
    }

    let id = Uuid::now_v7();
    let created = db
        .query(format!(
            "CREATE $id SET \
             receiving_mailbox = $receiving_mailbox, deployment = $deployment, \
             firm_id = $firm_id, provider_account = $provider_account, sender_authenticated = $sender_authenticated, \
             raw_digest = $raw_digest, source_message_id = $source_message_id, \
             archive_key = $archive_key, letter_id = $letter_id, \
             processing_state = $processing_state, delivery_state = $delivery_state \
             RETURN {SELECT}"
        ))
        .bind(("id", record_id(TABLE, id)))
        .bind(("receiving_mailbox", new.receiving_mailbox.to_string()))
        .bind(("deployment", new.deployment.to_string()))
        .bind((
            "firm_id",
            new.firm_id.map(|id| record_id(crate::firms::TABLE, id)),
        ))
        .bind(("provider_account", new.provider_account.map(str::to_string)))
        .bind(("sender_authenticated", new.sender_authenticated))
        .bind(("raw_digest", new.raw_digest.to_string()))
        .bind((
            "source_message_id",
            new.source_message_id.map(str::to_string),
        ))
        .bind(("archive_key", new.archive_key.to_string()))
        .bind(("letter_id", record_id(crate::letters::TABLE, new.letter_id)))
        .bind(("processing_state", PROCESSING_PENDING.to_string()))
        .bind(("delivery_state", DELIVERY_NOT_ATTEMPTED.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check);

    let mut response = match created {
        Ok(response) => response,
        Err(error) if is_duplicate(&error) => {
            let receipt = find(db, new.receiving_mailbox, new.deployment, new.raw_digest)
                .await?
                .ok_or(EmailReceiptError::Db(error))?;
            return Ok(Ensured {
                receipt,
                deduped: true,
            });
        }
        Err(error) => return Err(EmailReceiptError::Db(error)),
    };
    let row: Option<EmailReceiptRow> = response.take(0)?;
    let receipt = row
        .and_then(EmailReceiptRow::into_receipt)
        .ok_or(EmailReceiptError::WriteReturnedNothing)?;
    crate::postgres_mirror::after_write(
        db,
        crate::postgres_mirror::MirrorKey::EmailReceipt(receipt.id),
    )
    .await;
    Ok(Ensured {
        receipt,
        deduped: false,
    })
}

pub async fn find(
    db: &SurrealDb,
    receiving_mailbox: &str,
    deployment: &str,
    raw_digest: &str,
) -> Result<Option<EmailReceipt>, EmailReceiptError> {
    let mut response = db
        .query(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE receiving_mailbox = $receiving_mailbox \
             AND deployment = $deployment AND raw_digest = $raw_digest LIMIT 1"
        ))
        .bind(("receiving_mailbox", receiving_mailbox.to_string()))
        .bind(("deployment", deployment.to_string()))
        .bind(("raw_digest", raw_digest.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<EmailReceiptRow> = response.take(0)?;
    with_delivery_state(db, row).await
}

/// Find a receipt by its opaque durable-workflow key.
pub async fn find_by_id(
    db: &SurrealDb,
    id: Uuid,
) -> Result<Option<EmailReceipt>, EmailReceiptError> {
    let mut response = db
        .query(format!("SELECT {SELECT} FROM $id LIMIT 1"))
        .bind(("id", record_id(TABLE, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let row: Option<EmailReceiptRow> = response.take(0)?;
    with_delivery_state(db, row).await
}

// The delivery record is authoritative, including for receipts stored before
// a delivery existed. Reading it avoids a second writable copy of the status.
async fn with_delivery_state(
    db: &SurrealDb,
    row: Option<EmailReceiptRow>,
) -> Result<Option<EmailReceipt>, EmailReceiptError> {
    let Some(mut receipt) = row.and_then(EmailReceiptRow::into_receipt) else {
        return Ok(None);
    };
    if let Some(delivery) = crate::email_deliveries::find(db, receipt.id).await? {
        receipt.delivery_state = delivery.state;
    }
    Ok(Some(receipt))
}

pub async fn mark_archived(db: &SurrealDb, id: Uuid) -> Result<(), EmailReceiptError> {
    db.query("UPDATE $id SET processing_state = $state, updated_at = time::now()")
        .bind(("id", record_id(TABLE, id)))
        .bind(("state", PROCESSING_ARCHIVED.to_string()))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    crate::postgres_mirror::after_write(db, crate::postgres_mirror::MirrorKey::EmailReceipt(id))
        .await;
    Ok(())
}

/// Receipts past a caller-supplied deadline without authoritative confirmation.
/// Includes partial archives and missed triggers; contains no message content.
pub async fn list_overdue(
    db: &SurrealDb,
    before: DateTime<Utc>,
) -> Result<Vec<EmailReceipt>, EmailReceiptError> {
    let mut response = db.query(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE inserted_at < $before AND id NOT IN \
         (SELECT VALUE receipt_id FROM email_delivery WHERE state = 'confirmed') ORDER BY inserted_at"
    ))
        .bind(("before", surrealdb::types::Datetime::from(before)))
        .await.and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<EmailReceiptRow> = response.take(0)?;
    Ok(rows
        .into_iter()
        .filter_map(EmailReceiptRow::into_receipt)
        .collect())
}

pub async fn count(db: &SurrealDb) -> Result<i64, EmailReceiptError> {
    let mut response = db
        .query(format!("SELECT count() FROM {TABLE} GROUP ALL"))
        .await
        .and_then(surrealdb::IndexedResults::check)?;
    let rows: Vec<CountRow> = response.take(0)?;
    Ok(rows.first().map_or(0, |row| row.count))
}

fn is_duplicate(error: &surrealdb::Error) -> bool {
    crate::surreal::retry::unique_violation(error) == Some("email_receipt_identity")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identical_receipt_keys_share_one_record() {
        let db = crate::surreal::test_support::mem().await;
        let first = ensure(
            &db,
            &NewEmailReceipt {
                receiving_mailbox: "support@example.com",
                deployment: "staging",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "digest-a",
                source_message_id: Some("same@example.com"),
                archive_key: "inbound/digest-a.eml",
                letter_id: uuid::Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        let second = ensure(
            &db,
            &NewEmailReceipt {
                receiving_mailbox: "support@example.com",
                deployment: "staging",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "digest-a",
                source_message_id: Some("same@example.com"),
                archive_key: "inbound/digest-a.eml",
                letter_id: uuid::Uuid::now_v7(),
            },
        )
        .await
        .unwrap();

        assert!(!first.deduped);
        assert!(second.deduped);
        assert_eq!(first.receipt.id, second.receipt.id);
        assert_eq!(count(&db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn message_id_reuse_with_new_digest_is_distinct() {
        let db = crate::surreal::test_support::mem().await;
        let first = ensure(
            &db,
            &NewEmailReceipt {
                receiving_mailbox: "support@example.com",
                deployment: "staging",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "digest-a",
                source_message_id: Some("reused@example.com"),
                archive_key: "inbound/digest-a.eml",
                letter_id: uuid::Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
        let second = ensure(
            &db,
            &NewEmailReceipt {
                receiving_mailbox: "support@example.com",
                deployment: "staging",
                firm_id: None,
                provider_account: None,
                sender_authenticated: false,
                raw_digest: "digest-b",
                source_message_id: Some("reused@example.com"),
                archive_key: "inbound/digest-b.eml",
                letter_id: uuid::Uuid::now_v7(),
            },
        )
        .await
        .unwrap();

        assert_ne!(first.receipt.id, second.receipt.id);
        assert_eq!(count(&db).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn concurrent_identical_admissions_create_one_receipt() {
        let db = crate::surreal::test_support::mem().await;
        let new = NewEmailReceipt {
            receiving_mailbox: "support@example.com",
            deployment: "staging",
            firm_id: None,
            provider_account: None,
            sender_authenticated: false,
            raw_digest: "digest-race",
            source_message_id: None,
            archive_key: "inbound/digest-race.eml",
            letter_id: uuid::Uuid::now_v7(),
        };
        let (first, second) = tokio::join!(ensure(&db, &new), ensure(&db, &new));
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.receipt.id, second.receipt.id);
        assert_eq!(count(&db).await.unwrap(), 1);
        assert_ne!(first.deduped, second.deduped);
    }
}
