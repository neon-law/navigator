//! Thin `CronJob` trigger for recovering scheduled Blog Post submissions.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-social-publish-reconciler-trigger");
    let ingress =
        std::env::var("RESTATE_INGRESS_URL").context("RESTATE_INGRESS_URL must be set")?;
    let auth = std::env::var("RESTATE_AUTH_TOKEN").ok();
    let tick = chrono::Utc::now().format("%Y-%m-%d-%H-%M").to_string();
    workflows::start_workflow(
        &ingress,
        auth.as_deref(),
        "SocialPublishReconciler",
        &tick,
        "run",
        &workflows_service::social_publisher::ReconcileRequest { tick: tick.clone() },
        true,
    )
    .await
    .context("triggering scheduled social publication reconciliation")?;
    tracing::info!(workflow_service = "SocialPublishReconciler", %tick, "social publication reconciliation triggered");
    Ok(())
}
