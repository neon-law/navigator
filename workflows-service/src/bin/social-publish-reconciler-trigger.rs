//! Thin `CronJob` trigger for recovering scheduled Blog Post submissions.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-social-publish-reconciler-trigger");

    let now = chrono::Utc::now();
    let target = workflows::trigger::trigger_target(
        "NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED",
        |name| std::env::var(name).ok(),
    )
    .context("selecting the SocialPublishReconciler trigger ingress")?;
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
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
        }
        workflows::trigger::TriggerTarget::Restate {
            ingress,
            auth_token,
        } => {
            let tick = now.format("%Y-%m-%d-%H-%M").to_string();
            let _response = workflows::start_workflow(
                &ingress,
                auth_token.as_deref(),
                "SocialPublishReconciler",
                &tick,
                "run",
                &workflows_service::social_publisher::ReconcileRequest { tick: tick.clone() },
                true,
            )
            .await
            .context("triggering scheduled social publication reconciliation")?;
        }
    }

    tracing::info!(
        workflow_service = "SocialPublishReconciler",
        "social publication reconciliation triggered"
    );
    Ok(())
}
