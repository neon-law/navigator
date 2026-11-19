//! PostgreSQL ownership for an explicitly requested summary recovery operation.

use crate::postgres::PostgresDb;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error("the receipt was not found")]
    NotFound,
    #[error("another summary recovery operation owns this receipt")]
    Busy { operation: Uuid },
    #[error("the delivery outcome requires reconciliation before recovery")]
    Quarantined,
    #[error("a confirmed delivery requires an audited failure-notice authorization")]
    Confirmed,
    #[error("the recovery operator must be nonempty")]
    Actor,
    #[error("the summary request does not match the archived receipt")]
    Conflict,
    #[error("PostgreSQL refused summary recovery ownership")]
    Database(#[source] sqlx::Error),
}

/// The delivery authority permitted by the recorded source outcome.
#[derive(Clone, Copy)]
pub enum DeliveryRule<'a> {
    PreserveConfirmed,
    RetryFailureNotice(Option<&'a str>),
    Reconciled,
}

type DeliveryState = (String, Option<String>, Option<String>, bool);

/// Claim a receipt and admit its delivery in one transaction. A terminal or
/// explicitly parked previous owner must be verified by the durable host.
/// The same operation never reopens a delivery twice after a lost checkpoint.
///
/// # Errors
/// Refuses concurrent ownership, ambiguous delivery, unaudited confirmation, or SQL failure.
pub async fn claim(
    pg: &PostgresDb,
    receipt: Uuid,
    operation: Uuid,
    terminal_owner: Option<Uuid>,
    expected_digest: &str,
    rule: DeliveryRule<'_>,
) -> Result<bool, RecoveryError> {
    let mut tx = pg.pool().begin().await.map_err(RecoveryError::Database)?;
    let row: Option<(Option<Uuid>, String)> = sqlx::query_as(
        "SELECT summary_recovery_operation, raw_digest FROM email_receipt WHERE id=$1 FOR UPDATE",
    )
    .bind(receipt)
    .fetch_optional(&mut *tx)
    .await
    .map_err(RecoveryError::Database)?;
    let (owner, digest) = row.ok_or(RecoveryError::NotFound)?;
    if digest != expected_digest {
        return Err(RecoveryError::Conflict);
    }
    if owner == Some(operation) {
        tx.commit().await.map_err(RecoveryError::Database)?;
        return Ok(false);
    }
    if let Some(owner) = owner {
        if Some(owner) != terminal_owner {
            return Err(RecoveryError::Busy { operation: owner });
        }
    }
    let state: Option<DeliveryState> =
        sqlx::query_as("SELECT state,reconciled_by,reconciliation,reconciled_at IS NOT NULL FROM email_delivery WHERE receipt_id=$1 FOR UPDATE")
            .bind(receipt)
            .fetch_optional(&mut *tx)
            .await
            .map_err(RecoveryError::Database)?;
    if let Some(actor) = validate_delivery(state.as_ref(), rule)? {
        sqlx::query(
            "UPDATE email_delivery SET state='not_attempted', current_correlation_id=NULL, \
             slack_timestamp=NULL, last_error=NULL, reconciled_by=$2, \
             reconciliation='redrive_after_failed_result', reconciled_at=now(), updated_at=now() \
             WHERE receipt_id=$1 AND state='confirmed'",
        )
        .bind(receipt)
        .bind(actor)
        .execute(&mut *tx)
        .await
        .map_err(RecoveryError::Database)?;
    }
    sqlx::query("UPDATE email_receipt SET summary_recovery_operation=$2 WHERE id=$1")
        .bind(receipt)
        .bind(operation)
        .execute(&mut *tx)
        .await
        .map_err(RecoveryError::Database)?;
    tx.commit().await.map_err(RecoveryError::Database)?;
    Ok(true)
}

fn validate_delivery<'a>(
    state: Option<&DeliveryState>,
    rule: DeliveryRule<'a>,
) -> Result<Option<&'a str>, RecoveryError> {
    if matches!(rule, DeliveryRule::Reconciled) {
        let Some((state, actor, reason, recorded)) = state else {
            return Err(RecoveryError::Quarantined);
        };
        if state != "not_attempted"
            || !*recorded
            || actor.as_deref().is_none_or(|actor| actor.trim().is_empty())
            || reason.as_deref() != Some("operator_authorized_resend")
        {
            return Err(RecoveryError::Quarantined);
        }
        return Ok(None);
    }
    match state.map(|state| state.0.as_str()) {
        Some("confirmed") => match rule {
            DeliveryRule::PreserveConfirmed => Ok(None),
            DeliveryRule::RetryFailureNotice(actor) => {
                let actor = actor.ok_or(RecoveryError::Confirmed)?;
                if actor.trim().is_empty() {
                    return Err(RecoveryError::Actor);
                }
                Ok(Some(actor))
            }
            DeliveryRule::Reconciled => Err(RecoveryError::Quarantined),
        },
        None | Some("not_attempted" | "failed") => Ok(None),
        Some(_) => Err(RecoveryError::Quarantined),
    }
}

/// Read-only early rejection. [`claim`] repeats this under native row locks.
///
/// # Errors
/// Refuses wrong digest, unsafe delivery, or missing explicit reconciliation.
pub async fn preflight(
    pg: &PostgresDb,
    receipt: Uuid,
    expected_digest: &str,
    rule: DeliveryRule<'_>,
) -> Result<(), RecoveryError> {
    type PreflightRow = (String, Option<String>, Option<String>, Option<String>, bool);
    let row: Option<PreflightRow> = sqlx::query_as(
        "SELECT r.raw_digest,d.state,d.reconciled_by,d.reconciliation,d.reconciled_at IS NOT NULL FROM email_receipt r LEFT JOIN email_delivery d ON d.receipt_id=r.id WHERE r.id=$1")
        .bind(receipt).fetch_optional(pg.pool()).await.map_err(RecoveryError::Database)?;
    let (digest, state, actor, reason, recorded) = row.ok_or(RecoveryError::NotFound)?;
    if digest != expected_digest {
        return Err(RecoveryError::Conflict);
    }
    let state = state.map(|state| (state, actor, reason, recorded));
    validate_delivery(state.as_ref(), rule)?;
    Ok(())
}

/// Read the canonical operation pointer so the host can verify its terminal state
/// before requesting a CAS handoff through [`claim`].
///
/// # Errors
/// Returns a missing receipt or database failure.
pub async fn current_operation(
    pg: &PostgresDb,
    receipt: Uuid,
) -> Result<Option<Uuid>, RecoveryError> {
    sqlx::query_scalar("SELECT summary_recovery_operation FROM email_receipt WHERE id=$1")
        .bind(receipt)
        .fetch_optional(pg.pool())
        .await
        .map_err(RecoveryError::Database)?
        .ok_or(RecoveryError::NotFound)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod interrupted_tests;
