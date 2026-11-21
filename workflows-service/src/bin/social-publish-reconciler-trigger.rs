//! Thin `CronJob` trigger for recovering scheduled Blog Post submissions.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-social-publish-reconciler-trigger");

    let now = chrono::Utc::now();
    let target = workflows::trigger::trigger_target(|name| std::env::var(name).ok())
        .context("selecting the SocialPublishReconciler trigger ingress")?;
    let workflows::trigger::TriggerTarget { ingress, token } = target;

    // Slot-floored identity: the worker's internal reconciler
    // schedule already runs every five-minute slot, so the
    // CronJob's submit joins that run instead of starting a second
    // reconciler beside it.
    let slot = workflows_service::dbos_social_publisher::reconcile_slot(now);
    workflows::trigger::start_dbos_social_publish_reconciler(
        &ingress,
        &token,
        &serde_json::json!({
            "workflow_id":
                workflows::trigger::social_publish_reconciler_workflow_id(slot),
            "request": { "tick": slot.to_rfc3339() },
        }),
    )
    .await
    .context("triggering SocialPublishReconciler through DBOS ingress")?;

    tracing::info!(
        workflow_service = "SocialPublishReconciler",
        "social publication reconciliation triggered"
    );
    Ok(())
}
