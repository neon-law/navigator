//! Slack delivery state on the original inbound Message.

use serde::{Deserialize, Serialize};
use surrealdb::types::SurrealValue;
use uuid::Uuid;

use crate::inbox::InboxError;
use crate::postgres_mirror::{after_write, MirrorKey};
use crate::surreal::{record_id, record_uuid, SurrealDb};

/// A persisted reservation survives lost Slack responses and worker restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertReceipt {
    pub state: Option<String>,
    pub channel_id: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(SurrealValue)]
struct ReceiptRow {
    state: Option<String>,
    channel_id: Option<String>,
    timestamp: Option<String>,
}

/// Load the notice receipt retained on an inbound Message.
///
/// # Errors
/// Returns a database error or `NotFound` when the Message does not exist.
pub async fn receipt(db: &SurrealDb, message_id: Uuid) -> Result<AlertReceipt, InboxError> {
    let mut response = db
        .query("SELECT alert_state AS state, alert_channel_id AS channel_id, alert_timestamp AS timestamp FROM ONLY $id")
        .bind(("id", record_id("message", message_id)))
        .await?
        .check()?;
    let row: Option<ReceiptRow> = response.take(0)?;
    let row = row.ok_or(InboxError::NotFound)?;
    Ok(AlertReceipt {
        state: row.state,
        channel_id: row.channel_id,
        timestamp: row.timestamp,
    })
}

/// Reserve before the network call. Unknown results require reconciliation.
///
/// # Errors
/// Propagates database failures.
pub async fn reserve(
    db: &SurrealDb,
    message_id: Uuid,
    channel_id: &str,
) -> Result<bool, InboxError> {
    #[derive(SurrealValue)]
    struct Id {
        id: surrealdb::types::RecordId,
    }
    let mut response = db.query("UPDATE $id SET alert_state = 'unknown', alert_channel_id = $channel, updated_at = time::now() WHERE direction = 'inbound' AND alert_state = 'pending' RETURN id")
        .bind(("id", record_id("message", message_id)))
        .bind(("channel", channel_id.to_string())).await?.check()?;
    let ids: Vec<Id> = response.take(0)?;
    let reserved = ids
        .iter()
        .any(|row| record_uuid(&row.id) == Some(message_id));
    if reserved {
        after_write(db, MirrorKey::InboxMessage(message_id)).await;
    }
    Ok(reserved)
}

/// Record a notice result without regressing a confirmed delivery.
///
/// # Errors
/// Propagates database failures.
pub async fn record(
    db: &SurrealDb,
    message_id: Uuid,
    state: &str,
    timestamp: Option<&str>,
) -> Result<(), InboxError> {
    db.query("UPDATE $id SET alert_state = $state, alert_timestamp = $timestamp, updated_at = time::now() WHERE direction = 'inbound' AND (alert_state != 'notified' OR $state = 'notified')")
        .bind(("id", record_id("message", message_id)))
        .bind(("state", state.to_string()))
        .bind(("timestamp", timestamp.map(str::to_string))).await?.check()?;
    after_write(db, MirrorKey::InboxMessage(message_id)).await;
    Ok(())
}

/// New pending notices and ambiguous sends are recovered; historical rows are excluded.
///
/// # Errors
/// Propagates database failures.
pub async fn awaiting_reconciliation(db: &SurrealDb, limit: u32) -> Result<Vec<Uuid>, InboxError> {
    #[derive(SurrealValue)]
    struct Id {
        id: surrealdb::types::RecordId,
    }
    let mut response = db.query("SELECT id, alert_state, updated_at FROM message WHERE direction = 'inbound' AND alert_state IN ['pending', 'unknown'] ORDER BY alert_state ASC, updated_at ASC LIMIT $limit")
        .bind(("limit", i64::from(limit))).await?.check()?;
    let rows: Vec<Id> = response.take(0)?;
    Ok(rows.iter().filter_map(|row| record_uuid(&row.id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_reserved_alert_cannot_be_sent_again_and_historical_rows_are_not_replayed() {
        let db = crate::test_support::mem_surreal().await;
        let firm = crate::test_support::seed_firm_with_brand(&db, "neon").await;
        let incoming = crate::inbox::ingest_inbound(
            &db,
            &crate::inbox::IngestInbound {
                firm_id: firm,
                brand_key: Some("neon"),
                interaction_type: None,
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: None,
                channel: "email",
                provider_account: "support",
                provider_message_id: "synthetic-id",
                provider_thread_ref: None,
                external_address: "sender@example.com",
                external_name: None,
                subject: None,
                body: "Synthetic inquiry",
                source_page: None,
                source_channel: None,
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachment_asset_id: None,
                occurred_at: "2026-10-10T00:00:00Z",
            },
        )
        .await
        .unwrap();
        assert_eq!(
            awaiting_reconciliation(&db, 100).await.unwrap(),
            vec![incoming.message_id]
        );
        assert!(reserve(&db, incoming.message_id, "CSYNTHETIC")
            .await
            .unwrap());
        assert!(!reserve(&db, incoming.message_id, "CSYNTHETIC")
            .await
            .unwrap());
        assert_eq!(
            awaiting_reconciliation(&db, 100).await.unwrap(),
            vec![incoming.message_id]
        );
        assert_eq!(
            receipt(&db, incoming.message_id)
                .await
                .unwrap()
                .state
                .as_deref(),
            Some("unknown")
        );
        record(&db, incoming.message_id, "notified", Some("123.456"))
            .await
            .unwrap();
        assert_eq!(
            receipt(&db, incoming.message_id)
                .await
                .unwrap()
                .timestamp
                .as_deref(),
            Some("123.456")
        );
        record(&db, incoming.message_id, "unknown", None)
            .await
            .unwrap();
        let confirmed = receipt(&db, incoming.message_id).await.unwrap();
        assert_eq!(confirmed.state.as_deref(), Some("notified"));
        assert_eq!(confirmed.timestamp.as_deref(), Some("123.456"));
        db.query("UPDATE $id UNSET alert_state, alert_channel_id, alert_timestamp")
            .bind(("id", record_id("message", incoming.message_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert_eq!(receipt(&db, incoming.message_id).await.unwrap().state, None);
        assert!(awaiting_reconciliation(&db, 100).await.unwrap().is_empty());
    }
}
