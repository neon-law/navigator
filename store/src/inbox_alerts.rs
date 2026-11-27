//! Slack delivery state on the original inbound Message.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::inbox::InboxError;

/// A persisted reservation survives lost Slack responses and worker restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertReceipt {
    pub state: Option<String>,
    pub channel_id: Option<String>,
    pub timestamp: Option<String>,
}

/// Load the notice receipt retained on an inbound Message.
///
/// # Errors
/// Returns a database error or `NotFound` when the Message does not exist.
pub async fn receipt(_db: &ApplicationStore, message_id: Uuid) -> Result<AlertReceipt, InboxError> {
    let pg = crate::inbox::inbox_authority().await?;
    Box::pin(crate::postgres::inbox::alert_receipt_on(&pg, message_id)).await
}

/// Reserve before the network call. Unknown results require reconciliation.
///
/// # Errors
/// Propagates database failures.
pub async fn reserve(
    _db: &ApplicationStore,
    message_id: Uuid,
    channel_id: &str,
) -> Result<bool, InboxError> {
    let pg = crate::inbox::inbox_authority().await?;
    Box::pin(crate::postgres::inbox::reserve_alert_on(
        &pg, message_id, channel_id,
    ))
    .await
}

/// Record a notice result without regressing a confirmed delivery.
///
/// # Errors
/// Propagates database failures.
pub async fn record(
    _db: &ApplicationStore,
    message_id: Uuid,
    state: &str,
    timestamp: Option<&str>,
) -> Result<(), InboxError> {
    let pg = crate::inbox::inbox_authority().await?;
    Box::pin(crate::postgres::inbox::record_alert_on(
        &pg, message_id, state, timestamp,
    ))
    .await
}

/// New pending notices and ambiguous sends are recovered; historical rows are excluded.
///
/// # Errors
/// Propagates database failures.
pub async fn awaiting_reconciliation(
    _db: &ApplicationStore,
    limit: u32,
) -> Result<Vec<Uuid>, InboxError> {
    let pg = crate::inbox::inbox_authority().await?;
    Box::pin(crate::postgres::inbox::awaiting_alert_reconciliation_on(
        &pg, limit,
    ))
    .await
}

#[cfg(test)]
mod tests {}
