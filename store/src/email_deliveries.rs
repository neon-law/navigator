//! Durable Slack delivery state for inbound email summaries.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::communications::{self as authority, CommunicationsAuthorityError};
use crate::postgres::PostgresDb;

pub const NOT_ATTEMPTED: &str = "not_attempted";
pub const SENDING: &str = "sending";
pub const UNKNOWN: &str = "unknown";
pub const CONFIRMED: &str = "confirmed";
pub const FAILED: &str = "failed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDelivery {
    pub id: Uuid,
    pub receipt_id: Uuid,
    pub channel_id: String,
    pub state: String,
    pub current_correlation_id: Option<String>,
    pub slack_timestamp: Option<String>,
    pub last_error: Option<String>,
    pub reconciled_by: Option<String>,
    pub reconciliation: Option<String>,
    pub reconciled_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailDeliveryAttempt {
    pub id: Uuid,
    pub delivery_id: Uuid,
    pub correlation_id: String,
    pub state: String,
    pub slack_timestamp: Option<String>,
    pub error: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAdmission {
    pub delivery: EmailDelivery,
    pub attempt_id: Option<Uuid>,
}

struct DeliveryUpdate<'a> {
    state: &'a str,
    channel_id: Option<&'a str>,
    timestamp: Option<&'a str>,
    error: Option<&'a str>,
    actor: Option<&'a str>,
    reconciliation: Option<&'a str>,
}

#[derive(Debug, thiserror::Error)]
pub enum EmailDeliveryError {
    #[error(transparent)]
    Postgres(#[from] CommunicationsAuthorityError),
    #[error("delivery record was not found")]
    NotFound,
    #[error("delivery is not confirmed")]
    NotConfirmed,
    #[error("writing an email delivery returned no usable row")]
    WriteReturnedNothing,
}

pub async fn ensure(
    _db: &ApplicationStore,
    receipt_id: Uuid,
    channel_id: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    let pg = authority::authority().await?;
    // The unique `receipt_id` decides a race; the loser reads the winner.
    let query = sqlx::query(
        "INSERT INTO email_delivery (id, receipt_id, channel_id, state, inserted_at, \
         updated_at) VALUES ($1, $2, $3, $4, now(), now()) ON CONFLICT (receipt_id) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(receipt_id)
    .bind(channel_id.to_owned())
    .bind(NOT_ATTEMPTED);
    authority::execute(&pg, query).await?;
    find_pg(&pg, receipt_id)
        .await?
        .ok_or(EmailDeliveryError::WriteReturnedNothing)
}

pub async fn find(
    _db: &ApplicationStore,
    receipt_id: Uuid,
) -> Result<Option<EmailDelivery>, EmailDeliveryError> {
    let pg = authority::authority().await?;
    Ok(find_pg(&pg, receipt_id).await?)
}

/// Atomically reserve a send. A confirmed or unknown delivery is never
/// retried implicitly; the caller must reconcile an unknown state first.
pub async fn admit_attempt(
    _db: &ApplicationStore,
    receipt_id: Uuid,
    correlation_id: &str,
) -> Result<DeliveryAdmission, EmailDeliveryError> {
    let pg = authority::authority().await?;
    Box::pin(admit_attempt_pg(&pg, receipt_id, correlation_id)).await
}

pub async fn mark_confirmed(
    db: &ApplicationStore,
    receipt_id: Uuid,
    channel_id: &str,
    timestamp: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: CONFIRMED,
            channel_id: Some(channel_id),
            timestamp: Some(timestamp),
            error: None,
            actor: None,
            reconciliation: None,
        },
    )
    .await
}

pub async fn mark_unknown(
    db: &ApplicationStore,
    receipt_id: Uuid,
    error: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: UNKNOWN,
            channel_id: None,
            timestamp: None,
            error: Some(error),
            actor: None,
            reconciliation: None,
        },
    )
    .await
}

pub async fn mark_failed(
    db: &ApplicationStore,
    receipt_id: Uuid,
    error: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: FAILED,
            channel_id: None,
            timestamp: None,
            error: Some(error),
            actor: None,
            reconciliation: None,
        },
    )
    .await
}

pub async fn reconcile_confirmed(
    db: &ApplicationStore,
    receipt_id: Uuid,
    actor: &str,
    channel_id: &str,
    timestamp: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: CONFIRMED,
            channel_id: Some(channel_id),
            timestamp: Some(timestamp),
            error: None,
            actor: Some(actor),
            reconciliation: Some("operator_confirmed"),
        },
    )
    .await
}

pub async fn authorize_resend(
    db: &ApplicationStore,
    receipt_id: Uuid,
    actor: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: NOT_ATTEMPTED,
            channel_id: None,
            timestamp: None,
            error: None,
            actor: Some(actor),
            reconciliation: Some("operator_authorized_resend"),
        },
    )
    .await
}

/// Reopen a `confirmed` delivery whose posted message was a provider-failure
/// notice, so a redriven run may post the real summary. Refuses any other
/// state: an unknown delivery goes through [`authorize_resend`] instead.
pub async fn reopen_after_failed_result(
    db: &ApplicationStore,
    receipt_id: Uuid,
    actor: &str,
) -> Result<EmailDelivery, EmailDeliveryError> {
    let delivery = find(db, receipt_id)
        .await?
        .ok_or(EmailDeliveryError::NotFound)?;
    if delivery.state != CONFIRMED {
        return Err(EmailDeliveryError::NotConfirmed);
    }
    update_delivery(
        db,
        receipt_id,
        DeliveryUpdate {
            state: NOT_ATTEMPTED,
            channel_id: None,
            timestamp: None,
            error: None,
            actor: Some(actor),
            reconciliation: Some("redrive_after_failed_result"),
        },
    )
    .await
}

async fn update_delivery(
    _db: &ApplicationStore,
    receipt_id: Uuid,
    update: DeliveryUpdate<'_>,
) -> Result<EmailDelivery, EmailDeliveryError> {
    let pg = authority::authority().await?;
    Box::pin(update_delivery_pg(&pg, receipt_id, update)).await
}

/// List deliveries in one state, most-recently-updated first. The health and
/// operator reconciliation commands use this to surface ambiguous outcomes.
pub async fn list_by_state(
    _db: &ApplicationStore,
    state: &str,
) -> Result<Vec<EmailDelivery>, EmailDeliveryError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(d) AS row FROM email_delivery d WHERE d.state = $1 \
         ORDER BY d.updated_at DESC",
    )
    .bind(state.to_owned());
    Ok(authority::rows(&pg, query).await?)
}

pub async fn attempts(
    db: &ApplicationStore,
    receipt_id: Uuid,
) -> Result<Vec<EmailDeliveryAttempt>, EmailDeliveryError> {
    let delivery = find(db, receipt_id)
        .await?
        .ok_or(EmailDeliveryError::NotFound)?;
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(a) AS row FROM email_delivery_attempt a WHERE a.delivery_id = $1 \
         ORDER BY a.inserted_at, a.id",
    )
    .bind(delivery.id);
    Ok(authority::rows(&pg, query).await?)
}

fn find_pg(pg: &PostgresDb, receipt_id: Uuid) -> authority::Op<'_, Option<EmailDelivery>> {
    let query =
        sqlx::query("SELECT to_jsonb(d) AS row FROM email_delivery d WHERE d.receipt_id = $1")
            .bind(receipt_id);
    authority::maybe(pg, query)
}

fn row<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, EmailDeliveryError> {
    Ok(serde_json::from_value(value).map_err(CommunicationsAuthorityError::from)?)
}

/// The reservation and its attempt commit together; the state guard in the
/// `UPDATE` admits one sender when two race.
async fn admit_attempt_pg(
    pg: &PostgresDb,
    receipt_id: Uuid,
    correlation_id: &str,
) -> Result<DeliveryAdmission, EmailDeliveryError> {
    let sql = CommunicationsAuthorityError::from;
    let delivery = find_pg(pg, receipt_id)
        .await?
        .ok_or(EmailDeliveryError::NotFound)?;
    if delivery.state == CONFIRMED || delivery.state == UNKNOWN || delivery.state == SENDING {
        return Ok(DeliveryAdmission {
            delivery,
            attempt_id: None,
        });
    }
    let mut tx = pg.pool().begin().await.map_err(sql)?;
    let changed: Option<serde_json::Value> = sqlx::query_scalar(
        "UPDATE email_delivery d SET state = $2, current_correlation_id = $3, \
         last_error = NULL, updated_at = now() \
         WHERE d.id = $1 AND d.state IN ('not_attempted', 'failed') RETURNING to_jsonb(d)",
    )
    .bind(delivery.id)
    .bind(SENDING)
    .bind(correlation_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(sql)?;
    let Some(changed) = changed else {
        return Ok(DeliveryAdmission {
            delivery: find_pg(pg, receipt_id)
                .await?
                .ok_or(EmailDeliveryError::NotFound)?,
            attempt_id: None,
        });
    };
    let attempt_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO email_delivery_attempt (id, delivery_id, correlation_id, state, \
         inserted_at, updated_at) VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(attempt_id)
    .bind(delivery.id)
    .bind(correlation_id)
    .bind(SENDING)
    .execute(&mut *tx)
    .await
    .map_err(sql)?;
    tx.commit().await.map_err(sql)?;
    Ok(DeliveryAdmission {
        delivery: row(changed)?,
        attempt_id: Some(attempt_id),
    })
}

/// The delivery and its current attempt move together.
async fn update_delivery_pg(
    pg: &PostgresDb,
    receipt_id: Uuid,
    update: DeliveryUpdate<'_>,
) -> Result<EmailDelivery, EmailDeliveryError> {
    let sql = CommunicationsAuthorityError::from;
    let mut tx = pg.pool().begin().await.map_err(sql)?;
    let updated: Option<serde_json::Value> = sqlx::query_scalar(
        "UPDATE email_delivery d SET state = $2, channel_id = COALESCE($3, d.channel_id), \
         current_correlation_id = CASE WHEN $2 = 'not_attempted' THEN NULL \
         ELSE d.current_correlation_id END, \
         slack_timestamp = $4, last_error = $5, reconciled_by = $6, reconciliation = $7, \
         reconciled_at = CASE WHEN $6 IS NULL THEN d.reconciled_at ELSE now() END, \
         updated_at = now() WHERE d.receipt_id = $1 RETURNING to_jsonb(d)",
    )
    .bind(receipt_id)
    .bind(update.state)
    .bind(update.channel_id)
    .bind(update.timestamp)
    .bind(update.error)
    .bind(update.actor)
    .bind(update.reconciliation)
    .fetch_optional(&mut *tx)
    .await
    .map_err(sql)?;
    let updated: EmailDelivery = row(updated.ok_or(EmailDeliveryError::NotFound)?)?;
    if update.state != NOT_ATTEMPTED {
        if let Some(correlation_id) = updated.current_correlation_id.as_deref() {
            sqlx::query(
                "UPDATE email_delivery_attempt SET state = $1, slack_timestamp = $2, \
                 error = $3, updated_at = now() WHERE delivery_id = $4 AND correlation_id = $5",
            )
            .bind(update.state)
            .bind(update.timestamp)
            .bind(update.error)
            .bind(updated.id)
            .bind(correlation_id)
            .execute(&mut *tx)
            .await
            .map_err(sql)?;
        }
    }
    tx.commit().await.map_err(sql)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {}
