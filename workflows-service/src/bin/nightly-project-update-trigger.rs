//! Thin daily trigger for the Nightly Project Update.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-nightly-project-update-trigger");

    let run_id = workflows_service::nightly_project_update::run_key(chrono::Utc::now());
    let target = workflows::trigger::trigger_target(|name| std::env::var(name).ok())
        .context("selecting the NightlyProjectUpdate trigger ingress")?;
    let workflows::trigger::TriggerTarget { ingress, token } = target;

    workflows::trigger::start_dbos_nightly_project_update(
        &ingress,
        &token,
        &serde_json::json!({
            "workflow_id": workflows_service::dbos_nightly_project_update::workflow_id(&run_id)
        }),
    )
    .await
    .context("triggering NightlyProjectUpdate through DBOS ingress")?;

    tracing::info!(%run_id, "Nightly Project Update triggered");
    println!("triggered NightlyProjectUpdate/{run_id}");
    Ok(())
}
