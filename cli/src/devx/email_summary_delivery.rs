//! Read-only email pipeline health and audited Slack delivery reconciliation.
//!
//! The health check prints receipt IDs and bounded status words. Reconciliation
//! records operator decisions through the shared delivery store.

use anyhow::{Context, Result};
use uuid::Uuid;

/// Read-only health signal suitable for a scheduler: nonzero means attention.
pub fn health() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let db = store::surreal::connect_from_env().await?;
        let overdue = store::email_receipts::list_overdue(
            &db,
            chrono::Utc::now() - chrono::Duration::minutes(5),
        )
        .await?;
        let mut attention = !overdue.is_empty();
        for receipt in overdue {
            println!(
                "receipt={} outcome=overdue processing_state={}",
                receipt.id, receipt.processing_state
            );
        }
        for state in [
            store::email_deliveries::UNKNOWN,
            store::email_deliveries::FAILED,
        ] {
            for delivery in store::email_deliveries::list_by_state(&db, state).await? {
                attention = true;
                println!("receipt={} outcome={}", delivery.receipt_id, state);
            }
        }
        anyhow::ensure!(!attention, "email pipeline requires attention");
        println!("email pipeline healthy");
        Ok(())
    })
}

/// `navigator ops email-summary quarantined` — list every delivery
/// currently sitting in `UNKNOWN`, most-recently-updated first.
/// Also included in the health check for monitoring.
pub fn quarantined() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(quarantined_async())
}

async fn quarantined_async() -> Result<()> {
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to SurrealDB")?;
    let deliveries =
        store::email_deliveries::list_by_state(&surreal, store::email_deliveries::UNKNOWN)
            .await
            .context("list quarantined deliveries")?;
    if deliveries.is_empty() {
        println!("no quarantined deliveries");
        return Ok(());
    }
    for delivery in deliveries {
        println!(
            "receipt={} channel={} last_error={} updated_at={}",
            delivery.receipt_id,
            delivery.channel_id,
            delivery.last_error.as_deref().unwrap_or("(none)"),
            delivery.updated_at,
        );
    }
    Ok(())
}

/// `navigator ops email-summary reconcile-confirmed --receipt <uuid> --actor
/// <actor> --channel <id> --timestamp <ts>` — record that a quarantined
/// send actually reached Slack, so replay stops treating it as in doubt.
pub fn reconcile_confirmed(
    receipt: Uuid,
    actor: String,
    channel: String,
    timestamp: String,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(reconcile_confirmed_async(
        receipt, actor, channel, timestamp,
    ))
}

async fn reconcile_confirmed_async(
    receipt_id: Uuid,
    actor: String,
    channel_id: String,
    timestamp: String,
) -> Result<()> {
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to SurrealDB")?;
    let delivery = store::email_deliveries::reconcile_confirmed(
        &surreal,
        receipt_id,
        &actor,
        &channel_id,
        &timestamp,
    )
    .await
    .context("reconcile the delivery as confirmed")?;
    println!("receipt={} state={}", delivery.receipt_id, delivery.state);
    Ok(())
}

/// `navigator ops email-summary authorize-resend --receipt <uuid> --actor
/// <actor>` — record operator authorization to resend a quarantined
/// delivery. Only unblocks the next `admit_attempt`; it does not itself
/// resend anything.
pub fn authorize_resend(receipt: Uuid, actor: String) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(authorize_resend_async(receipt, actor))
}

async fn authorize_resend_async(receipt_id: Uuid, actor: String) -> Result<()> {
    let surreal = store::surreal::connect_from_env()
        .await
        .context("connect to SurrealDB")?;
    let delivery = store::email_deliveries::authorize_resend(&surreal, receipt_id, &actor)
        .await
        .context("authorize the resend")?;
    println!("receipt={} state={}", delivery.receipt_id, delivery.state);
    Ok(())
}
