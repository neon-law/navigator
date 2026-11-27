//! Queue a receipt-preserving DBOS summary recovery operation.

use anyhow::{Context, Result};
use uuid::Uuid;

/// `hudson ops email-summary redrive --receipt <uuid>`.
pub fn run(
    receipt: Uuid,
    after_failed_result: Option<&str>,
    recovery_id: Option<Uuid>,
    from_recovery: Option<Uuid>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(redrive(
        receipt,
        after_failed_result,
        recovery_id,
        from_recovery,
    ))
}

async fn redrive(
    receipt_id: Uuid,
    after_failed_result: Option<&str>,
    recovery_id: Option<Uuid>,
    from_recovery: Option<Uuid>,
) -> Result<()> {
    let application_store = crate::application_store()
        .await
        .context("verify PostgreSQL application authority")?;
    let config = workflows::EmailSummaryConfig::from_env()
        .context("configure inbound email summary lane")?
        .with_context(|| "NAVIGATOR_SUMMARY_ENABLED is not set; nothing to redrive against")?;
    let (ingress, token) = workflows::trigger::dbos_ingress_from_env()
        .context("configure the summary recovery ingress")?;
    let operation_id = recovery_id
        .context("--recovery-id is required for DBOS; reuse it when retrying this operation")?;
    let receipt = store::email_receipts::find_by_id(&application_store, receipt_id)
        .await
        .context("load the receipt")?
        .context("receipt not found")?;
    let request = config
        .request_for(receipt_id, &receipt.raw_digest)
        .context("build the summary request")?;
    workflows::trigger::start_dbos_email_summary_recovery(
        &ingress,
        &token,
        &workflows::trigger::EmailSummaryRecoveryRequest {
            operation_id,
            from_recovery,
            after_failed_result: after_failed_result.map(str::to_owned),
            request,
        },
    )
    .await
    .context("queue DBOS recovery")?;
    println!("EmailSummary recovery queued");
    Ok(())
}
