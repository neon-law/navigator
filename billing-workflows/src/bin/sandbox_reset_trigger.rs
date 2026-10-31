//! `sandbox-credit-reset trigger` — the thin monthly `CronJob` entrypoint.
//!
//! Fires one `SandboxCreditReset` invocation against the Restate ingress,
//! then exits. The UTC billing month names the run: a same-month re-fire
//! joins the same keyed run instead of granting or invoicing twice, and
//! next month's fire is a new run. Restate owns the retry schedule.
//!
//! Auth + env handling are identical to the reconcile trigger
//! (`src/bin/reconcile_trigger.rs`).

use anyhow::{Context, Result};
use chrono::Datelike;

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    // One observability seam for every binary: stdout logs (JSON when an
    // OTLP endpoint is set) plus OTLP traces + metrics. Held to end of main
    // so the drop flushes any batched export before the process exits.
    let _telemetry = telemetry::init("navigator-sandbox-credit-reset-trigger");

    let now = chrono::Utc::now();
    let run_id = store::sandbox_usage::billing_period(now.year(), now.month());
    let target =
        workflows::trigger::trigger_target("NAVIGATOR_DBOS_SANDBOX_CREDIT_RESET_ENABLED", |name| {
            std::env::var(name).ok()
        })
        .context("selecting the SandboxCreditReset trigger ingress")?;
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_reconcile(
                &ingress,
                &token,
                &serde_json::json!({
                    "workflow_id": format!("SandboxCreditReset:{run_id}")
                }),
            )
            .await
            .context("triggering SandboxCreditReset through DBOS ingress")?;
        }
        workflows::trigger::TriggerTarget::Restate {
            ingress,
            auth_token,
        } => {
            let _response = workflows::start_workflow(
                &ingress,
                auth_token.as_deref(),
                "SandboxCreditReset",
                &run_id,
                "run",
                &serde_json::json!({}),
                true, // one-way: accept the invocation and exit; Restate runs it.
            )
            .await
            .context("triggering SandboxCreditReset workflow")?;
        }
    }

    tracing::info!(%run_id, "sandbox-credit-reset workflow triggered");
    println!("triggered SandboxCreditReset/{run_id}");
    Ok(())
}
