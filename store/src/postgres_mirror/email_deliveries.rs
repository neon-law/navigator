//! PostgreSQL mirror for inbound Email Receipts and their Slack delivery records.
//!
//! SurrealDB remains authoritative. The receipt identity preserves inbound
//! deduplication, while delivery and attempt rows keep the durable dispatch
//! state and provider outcomes. Mirror errors identify rows only by UUID.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{postgres::PgRow, Row};
use surrealdb::types::{Datetime, RecordId, SurrealValue};
use uuid::Uuid;

use super::{changed, micros, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{
    postgres::PostgresDb,
    surreal::{record_uuid, SurrealDb},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReceiptImage {
    id: Uuid,
    receiving_mailbox: String,
    deployment: String,
    firm_id: Option<Uuid>,
    provider_account: Option<String>,
    sender_authenticated: bool,
    raw_digest: String,
    source_message_id: Option<String>,
    archive_key: String,
    letter_id: Uuid,
    processing_state: String,
    delivery_state: String,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeliveryImage {
    id: Uuid,
    receipt_id: Uuid,
    channel_id: String,
    state: String,
    current_correlation_id: Option<String>,
    slack_timestamp: Option<String>,
    last_error: Option<String>,
    reconciled_by: Option<String>,
    reconciliation: Option<String>,
    reconciled_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AttemptImage {
    id: Uuid,
    delivery_id: Uuid,
    correlation_id: String,
    state: String,
    slack_timestamp: Option<String>,
    error: Option<String>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(SurrealValue)]
struct ReceiptSource {
    id: RecordId,
    receiving_mailbox: String,
    deployment: String,
    firm_id: Option<RecordId>,
    provider_account: Option<String>,
    sender_authenticated: Option<bool>,
    raw_digest: String,
    source_message_id: Option<String>,
    archive_key: String,
    letter_id: RecordId,
    processing_state: String,
    delivery_state: String,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(SurrealValue)]
struct DeliverySource {
    id: RecordId,
    receipt_id: RecordId,
    channel_id: String,
    state: String,
    current_correlation_id: Option<String>,
    slack_timestamp: Option<String>,
    last_error: Option<String>,
    reconciled_by: Option<String>,
    reconciliation: Option<String>,
    reconciled_at: Option<Datetime>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

#[derive(SurrealValue)]
struct AttemptSource {
    id: RecordId,
    delivery_id: RecordId,
    correlation_id: String,
    state: String,
    slack_timestamp: Option<String>,
    error: Option<String>,
    inserted_at: Datetime,
    updated_at: Datetime,
}

const RECEIPT_FIELDS: &str = "id, receiving_mailbox, deployment, firm_id, provider_account, sender_authenticated, raw_digest, source_message_id, archive_key, letter_id, processing_state, delivery_state, inserted_at, updated_at";
const DELIVERY_FIELDS: &str = "id, receipt_id, channel_id, state, current_correlation_id, slack_timestamp, last_error, reconciled_by, reconciliation, reconciled_at, inserted_at, updated_at";
const ATTEMPT_FIELDS: &str =
    "id, delivery_id, correlation_id, state, slack_timestamp, error, inserted_at, updated_at";

fn uuid(id: &RecordId) -> Result<Uuid, MirrorError> {
    record_uuid(id).ok_or(MirrorError::Source)
}

fn optional_uuid(id: Option<&RecordId>) -> Result<Option<Uuid>, MirrorError> {
    id.map(uuid).transpose()
}

impl TryFrom<ReceiptSource> for ReceiptImage {
    type Error = MirrorError;

    fn try_from(row: ReceiptSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            receiving_mailbox: row.receiving_mailbox,
            deployment: row.deployment,
            firm_id: optional_uuid(row.firm_id.as_ref())?,
            provider_account: row.provider_account,
            sender_authenticated: row.sender_authenticated.unwrap_or(false),
            raw_digest: row.raw_digest,
            source_message_id: row.source_message_id,
            archive_key: row.archive_key,
            letter_id: uuid(&row.letter_id)?,
            processing_state: row.processing_state,
            delivery_state: row.delivery_state,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

impl TryFrom<DeliverySource> for DeliveryImage {
    type Error = MirrorError;

    fn try_from(row: DeliverySource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            receipt_id: uuid(&row.receipt_id)?,
            channel_id: row.channel_id,
            state: row.state,
            current_correlation_id: row.current_correlation_id,
            slack_timestamp: row.slack_timestamp,
            last_error: row.last_error,
            reconciled_by: row.reconciled_by,
            reconciliation: row.reconciliation,
            reconciled_at: row.reconciled_at.map(Into::into).map(micros),
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

impl TryFrom<AttemptSource> for AttemptImage {
    type Error = MirrorError;

    fn try_from(row: AttemptSource) -> Result<Self, Self::Error> {
        Ok(Self {
            id: uuid(&row.id)?,
            delivery_id: uuid(&row.delivery_id)?,
            correlation_id: row.correlation_id,
            state: row.state,
            slack_timestamp: row.slack_timestamp,
            error: row.error,
            inserted_at: micros(row.inserted_at.into()),
            updated_at: micros(row.updated_at.into()),
        })
    }
}

async fn source_rows<T: SurrealValue>(
    surreal: &SurrealDb,
    table: &str,
    fields: &str,
) -> Result<Vec<T>, MirrorError> {
    surreal
        .query(format!("SELECT {fields} FROM {table}"))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)
}

async fn source_one<T: SurrealValue>(
    surreal: &SurrealDb,
    table: &str,
    fields: &str,
    id: Uuid,
) -> Result<Option<T>, MirrorError> {
    let mut response = surreal
        .query(format!("SELECT {fields} FROM ONLY $id LIMIT 1"))
        .bind(("id", crate::surreal::record_id(table, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?;
    response.take(0).map_err(|_| MirrorError::Source)
}

async fn source(surreal: &SurrealDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let mut rows = BTreeMap::new();
    for row in source_rows::<ReceiptSource>(surreal, "email_receipt", RECEIPT_FIELDS).await? {
        let image = ReceiptImage::try_from(row)?;
        rows.insert(
            MirrorKey::EmailReceipt(image.id),
            Columns::EmailReceipt(Box::new(image)),
        );
    }
    for row in source_rows::<DeliverySource>(surreal, "email_delivery", DELIVERY_FIELDS).await? {
        let image = DeliveryImage::try_from(row)?;
        rows.insert(
            MirrorKey::EmailDelivery(image.id),
            Columns::EmailDelivery(Box::new(image)),
        );
    }
    for row in
        source_rows::<AttemptSource>(surreal, "email_delivery_attempt", ATTEMPT_FIELDS).await?
    {
        let image = AttemptImage::try_from(row)?;
        rows.insert(
            MirrorKey::EmailDeliveryAttempt(image.id),
            Columns::EmailDeliveryAttempt(Box::new(image)),
        );
    }
    Ok(rows)
}

fn receipt_from_row(row: &PgRow) -> Result<ReceiptImage, sqlx::Error> {
    Ok(ReceiptImage {
        id: row.try_get("id")?,
        receiving_mailbox: row.try_get("receiving_mailbox")?,
        deployment: row.try_get("deployment")?,
        firm_id: row.try_get("firm_id")?,
        provider_account: row.try_get("provider_account")?,
        sender_authenticated: row.try_get("sender_authenticated")?,
        raw_digest: row.try_get("raw_digest")?,
        source_message_id: row.try_get("source_message_id")?,
        archive_key: row.try_get("archive_key")?,
        letter_id: row.try_get("letter_id")?,
        processing_state: row.try_get("processing_state")?,
        delivery_state: row.try_get("delivery_state")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn delivery_from_row(row: &PgRow) -> Result<DeliveryImage, sqlx::Error> {
    Ok(DeliveryImage {
        id: row.try_get("id")?,
        receipt_id: row.try_get("receipt_id")?,
        channel_id: row.try_get("channel_id")?,
        state: row.try_get("state")?,
        current_correlation_id: row.try_get("current_correlation_id")?,
        slack_timestamp: row.try_get("slack_timestamp")?,
        last_error: row.try_get("last_error")?,
        reconciled_by: row.try_get("reconciled_by")?,
        reconciliation: row.try_get("reconciliation")?,
        reconciled_at: row.try_get("reconciled_at")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn attempt_from_row(row: &PgRow) -> Result<AttemptImage, sqlx::Error> {
    Ok(AttemptImage {
        id: row.try_get("id")?,
        delivery_id: row.try_get("delivery_id")?,
        correlation_id: row.try_get("correlation_id")?,
        state: row.try_get("state")?,
        slack_timestamp: row.try_get("slack_timestamp")?,
        error: row.try_get("error")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

async fn target(pg: &PostgresDb) -> Result<BTreeMap<MirrorKey, Columns>, MirrorError> {
    let mut rows = BTreeMap::new();
    for row in sqlx::query("SELECT id, receiving_mailbox, deployment, firm_id, provider_account, sender_authenticated, raw_digest, source_message_id, archive_key, letter_id, processing_state, delivery_state, inserted_at, updated_at FROM email_receipt")
        .fetch_all(&pg.0)
        .await?
    {
        let image = receipt_from_row(&row)?;
        rows.insert(
            MirrorKey::EmailReceipt(image.id),
            Columns::EmailReceipt(Box::new(image)),
        );
    }
    for row in sqlx::query("SELECT id, receipt_id, channel_id, state, current_correlation_id, slack_timestamp, last_error, reconciled_by, reconciliation, reconciled_at, inserted_at, updated_at FROM email_delivery")
        .fetch_all(&pg.0)
        .await?
    {
        let image = delivery_from_row(&row)?;
        rows.insert(
            MirrorKey::EmailDelivery(image.id),
            Columns::EmailDelivery(Box::new(image)),
        );
    }
    for row in sqlx::query("SELECT id, delivery_id, correlation_id, state, slack_timestamp, error, inserted_at, updated_at FROM email_delivery_attempt")
        .fetch_all(&pg.0)
        .await?
    {
        let image = attempt_from_row(&row)?;
        rows.insert(
            MirrorKey::EmailDeliveryAttempt(image.id),
            Columns::EmailDeliveryAttempt(Box::new(image)),
        );
    }
    Ok(rows)
}

async fn upsert_receipt(pg: &PostgresDb, row: &ReceiptImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO email_receipt (id, receiving_mailbox, deployment, firm_id, provider_account, sender_authenticated, raw_digest, source_message_id, archive_key, letter_id, processing_state, delivery_state, inserted_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) ON CONFLICT (id) DO UPDATE SET receiving_mailbox=EXCLUDED.receiving_mailbox, deployment=EXCLUDED.deployment, firm_id=EXCLUDED.firm_id, provider_account=EXCLUDED.provider_account, sender_authenticated=EXCLUDED.sender_authenticated, raw_digest=EXCLUDED.raw_digest, source_message_id=EXCLUDED.source_message_id, archive_key=EXCLUDED.archive_key, letter_id=EXCLUDED.letter_id, processing_state=EXCLUDED.processing_state, delivery_state=EXCLUDED.delivery_state, inserted_at=EXCLUDED.inserted_at, updated_at=EXCLUDED.updated_at WHERE (email_receipt.receiving_mailbox,email_receipt.deployment,email_receipt.firm_id,email_receipt.provider_account,email_receipt.sender_authenticated,email_receipt.raw_digest,email_receipt.source_message_id,email_receipt.archive_key,email_receipt.letter_id,email_receipt.processing_state,email_receipt.delivery_state,email_receipt.inserted_at,email_receipt.updated_at) IS DISTINCT FROM (EXCLUDED.receiving_mailbox,EXCLUDED.deployment,EXCLUDED.firm_id,EXCLUDED.provider_account,EXCLUDED.sender_authenticated,EXCLUDED.raw_digest,EXCLUDED.source_message_id,EXCLUDED.archive_key,EXCLUDED.letter_id,EXCLUDED.processing_state,EXCLUDED.delivery_state,EXCLUDED.inserted_at,EXCLUDED.updated_at)",
    )
    .bind(row.id)
    .bind(&row.receiving_mailbox)
    .bind(&row.deployment)
    .bind(row.firm_id)
    .bind(&row.provider_account)
    .bind(row.sender_authenticated)
    .bind(&row.raw_digest)
    .bind(&row.source_message_id)
    .bind(&row.archive_key)
    .bind(row.letter_id)
    .bind(&row.processing_state)
    .bind(&row.delivery_state)
    .bind(micros(row.inserted_at))
    .bind(micros(row.updated_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn upsert_delivery(pg: &PostgresDb, row: &DeliveryImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO email_delivery (id, receipt_id, channel_id, state, current_correlation_id, slack_timestamp, last_error, reconciled_by, reconciliation, reconciled_at, inserted_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) ON CONFLICT (id) DO UPDATE SET receipt_id=EXCLUDED.receipt_id, channel_id=EXCLUDED.channel_id, state=EXCLUDED.state, current_correlation_id=EXCLUDED.current_correlation_id, slack_timestamp=EXCLUDED.slack_timestamp, last_error=EXCLUDED.last_error, reconciled_by=EXCLUDED.reconciled_by, reconciliation=EXCLUDED.reconciliation, reconciled_at=EXCLUDED.reconciled_at, inserted_at=EXCLUDED.inserted_at, updated_at=EXCLUDED.updated_at WHERE (email_delivery.receipt_id,email_delivery.channel_id,email_delivery.state,email_delivery.current_correlation_id,email_delivery.slack_timestamp,email_delivery.last_error,email_delivery.reconciled_by,email_delivery.reconciliation,email_delivery.reconciled_at,email_delivery.inserted_at,email_delivery.updated_at) IS DISTINCT FROM (EXCLUDED.receipt_id,EXCLUDED.channel_id,EXCLUDED.state,EXCLUDED.current_correlation_id,EXCLUDED.slack_timestamp,EXCLUDED.last_error,EXCLUDED.reconciled_by,EXCLUDED.reconciliation,EXCLUDED.reconciled_at,EXCLUDED.inserted_at,EXCLUDED.updated_at)",
    )
    .bind(row.id)
    .bind(row.receipt_id)
    .bind(&row.channel_id)
    .bind(&row.state)
    .bind(&row.current_correlation_id)
    .bind(&row.slack_timestamp)
    .bind(&row.last_error)
    .bind(&row.reconciled_by)
    .bind(&row.reconciliation)
    .bind(row.reconciled_at.map(micros))
    .bind(micros(row.inserted_at))
    .bind(micros(row.updated_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn upsert_attempt(pg: &PostgresDb, row: &AttemptImage) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO email_delivery_attempt (id, delivery_id, correlation_id, state, slack_timestamp, error, inserted_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) ON CONFLICT (id) DO UPDATE SET delivery_id=EXCLUDED.delivery_id, correlation_id=EXCLUDED.correlation_id, state=EXCLUDED.state, slack_timestamp=EXCLUDED.slack_timestamp, error=EXCLUDED.error, inserted_at=EXCLUDED.inserted_at, updated_at=EXCLUDED.updated_at WHERE (email_delivery_attempt.delivery_id,email_delivery_attempt.correlation_id,email_delivery_attempt.state,email_delivery_attempt.slack_timestamp,email_delivery_attempt.error,email_delivery_attempt.inserted_at,email_delivery_attempt.updated_at) IS DISTINCT FROM (EXCLUDED.delivery_id,EXCLUDED.correlation_id,EXCLUDED.state,EXCLUDED.slack_timestamp,EXCLUDED.error,EXCLUDED.inserted_at,EXCLUDED.updated_at)",
    )
    .bind(row.id)
    .bind(row.delivery_id)
    .bind(&row.correlation_id)
    .bind(&row.state)
    .bind(&row.slack_timestamp)
    .bind(&row.error)
    .bind(micros(row.inserted_at))
    .bind(micros(row.updated_at))
    .execute(&pg.0)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
}

async fn ensure_firm(surreal: &SurrealDb, pg: &PostgresDb, id: Uuid) -> Result<(), MirrorError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM firm WHERE id=$1)")
        .bind(id)
        .fetch_one(&pg.0)
        .await?;
    if !exists {
        super::tenancy::mirror(surreal, pg, &MirrorKey::Firm(id)).await?;
    }
    Ok(())
}

async fn ensure_receipt(surreal: &SurrealDb, pg: &PostgresDb, id: Uuid) -> Result<(), MirrorError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM email_receipt WHERE id=$1)")
            .bind(id)
            .fetch_one(&pg.0)
            .await?;
    if !exists {
        if let Some(row) =
            source_one::<ReceiptSource>(surreal, "email_receipt", RECEIPT_FIELDS, id).await?
        {
            let image = ReceiptImage::try_from(row)?;
            if let Some(firm_id) = image.firm_id {
                ensure_firm(surreal, pg, firm_id).await?;
            }
            upsert_receipt(pg, &image).await?;
        }
    }
    Ok(())
}

async fn ensure_delivery(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    id: Uuid,
) -> Result<(), MirrorError> {
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM email_delivery WHERE id=$1)")
            .bind(id)
            .fetch_one(&pg.0)
            .await?;
    if !exists {
        if let Some(row) =
            source_one::<DeliverySource>(surreal, "email_delivery", DELIVERY_FIELDS, id).await?
        {
            let image = DeliveryImage::try_from(row)?;
            ensure_receipt(surreal, pg, image.receipt_id).await?;
            upsert_delivery(pg, &image).await?;
        }
    }
    Ok(())
}

pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    key: &MirrorKey,
) -> Result<Mirrored, MirrorError> {
    match key {
        MirrorKey::EmailReceipt(id) => {
            let Some(row) =
                source_one::<ReceiptSource>(surreal, "email_receipt", RECEIPT_FIELDS, *id).await?
            else {
                return Ok(changed(
                    sqlx::query("DELETE FROM email_receipt WHERE id=$1")
                        .bind(id)
                        .execute(&pg.0)
                        .await?
                        .rows_affected(),
                    Mirrored::Deleted,
                ));
            };
            let image = ReceiptImage::try_from(row)?;
            if let Some(firm_id) = image.firm_id {
                ensure_firm(surreal, pg, firm_id).await?;
            }
            upsert_receipt(pg, &image).await
        }
        MirrorKey::EmailDelivery(id) => {
            let Some(row) =
                source_one::<DeliverySource>(surreal, "email_delivery", DELIVERY_FIELDS, *id)
                    .await?
            else {
                return Ok(changed(
                    sqlx::query("DELETE FROM email_delivery WHERE id=$1")
                        .bind(id)
                        .execute(&pg.0)
                        .await?
                        .rows_affected(),
                    Mirrored::Deleted,
                ));
            };
            let image = DeliveryImage::try_from(row)?;
            ensure_receipt(surreal, pg, image.receipt_id).await?;
            upsert_delivery(pg, &image).await
        }
        MirrorKey::EmailDeliveryAttempt(id) => {
            let Some(row) =
                source_one::<AttemptSource>(surreal, "email_delivery_attempt", ATTEMPT_FIELDS, *id)
                    .await?
            else {
                return Ok(changed(
                    sqlx::query("DELETE FROM email_delivery_attempt WHERE id=$1")
                        .bind(id)
                        .execute(&pg.0)
                        .await?
                        .rows_affected(),
                    Mirrored::Deleted,
                ));
            };
            let image = AttemptImage::try_from(row)?;
            ensure_delivery(surreal, pg, image.delivery_id).await?;
            upsert_attempt(pg, &image).await
        }
        _ => Err(MirrorError::Source),
    }
}

pub async fn parity(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let source = source(surreal).await?;
    let target = target(pg).await?;
    Ok(ParityReport {
        tables: ["email_receipt", "email_delivery", "email_delivery_attempt"]
            .into_iter()
            .map(|table| {
                super::compare(
                    table,
                    &for_table(&source, table),
                    &for_table(&target, table),
                )
            })
            .collect(),
    })
}

fn for_table(
    rows: &BTreeMap<MirrorKey, Columns>,
    table: &'static str,
) -> BTreeMap<MirrorKey, Columns> {
    rows.iter()
        .filter(|(key, _)| key.table() == table)
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

pub async fn reconcile(surreal: &SurrealDb, pg: &PostgresDb) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg).await?;
    for key in before.divergent() {
        if let Err(error) = mirror(surreal, pg, key).await {
            tracing::warn!(key = %key, error = %error, "PostgreSQL reconcile could not mirror the row");
        }
    }
    parity(surreal, pg).await
}

#[cfg(test)]
mod tests;
