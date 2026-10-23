//! Thin daily trigger for the Nightly Project Update.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-nightly-project-update-trigger");

    let run_id = workflows_service::nightly_project_update::run_key(chrono::Utc::now());
    let target = workflows::trigger::trigger_target(
        "NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED",
        |name| std::env::var(name).ok(),
    )
    .context("selecting the NightlyProjectUpdate trigger ingress")?;
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_nightly_project_update(
                &ingress,
                &token,
                &serde_json::json!({
                    "workflow_id": workflows_service::dbos_nightly_project_update::workflow_id(&run_id)
                }),
            )
            .await
            .context("triggering NightlyProjectUpdate through DBOS ingress")?;
        }
        workflows::trigger::TriggerTarget::Restate {
            ingress,
            auth_token,
        } => {
            let _response = workflows::start_workflow(
                &ingress,
                auth_token.as_deref(),
                "NightlyProjectUpdate",
                &run_id,
                "run",
                &serde_json::json!({}),
                true,
            )
            .await
            .context("triggering NightlyProjectUpdate workflow")?;
        }
    }

    tracing::info!(%run_id, "Nightly Project Update triggered");
    println!("triggered NightlyProjectUpdate/{run_id}");
    Ok(())
}
