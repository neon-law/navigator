//! `billing-reconcile trigger` — the thin nightly `CronJob` entrypoint.
//!
//! Fires one `ReconcileInvoices` invocation against the selected durable
//! ingress, then exits. The UTC run date names the same idempotent run on both
//! engines. `NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED=true` selects DBOS and
//! requires its authenticated worker ingress; otherwise the trigger uses
//! Restate during the transition.
//!
//! Auth + env handling are identical to the canary trigger
//! (`src/bin/trigger.rs`): the shared trigger helpers attach only the
//! credential for the selected ingress.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    // One observability seam for every binary: stdout logs (JSON when an
    // OTLP endpoint is set) plus OTLP traces + metrics. Held to end of main
    // so the drop flushes any batched export before the process exits.
    let _telemetry = telemetry::init("navigator-reconcile-invoices-trigger");

    let run_id = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let target =
        workflows::trigger::trigger_target("NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED", |name| {
            std::env::var(name).ok()
        })
        .context("selecting the ReconcileInvoices trigger ingress")?;
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_reconcile(
                &ingress,
                &token,
                &serde_json::json!({
                    "workflow_id": format!("ReconcileInvoices:{run_id}")
                }),
            )
            .await
            .context("triggering ReconcileInvoices through DBOS ingress")?;
        }
        workflows::trigger::TriggerTarget::Restate {
            ingress,
            auth_token,
        } => {
            let _response = workflows::start_workflow(
                &ingress,
                auth_token.as_deref(),
                "ReconcileInvoices",
                &run_id,
                "run",
                &serde_json::json!({}),
                true, // one-way: accept the invocation and exit; Restate runs it.
            )
            .await
            .context("triggering ReconcileInvoices workflow")?;
        }
    }

    tracing::info!(%run_id, "reconcile-invoices workflow triggered");
    println!("triggered ReconcileInvoices/{run_id}");
    Ok(())
}
