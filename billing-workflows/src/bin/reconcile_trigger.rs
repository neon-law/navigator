//! `billing-reconcile trigger` — the thin nightly `CronJob` entrypoint.
//!
//! Fires one `ReconcileInvoices` invocation against the selected durable
//! ingress, then exits. The UTC run date names the same idempotent run on both
//! engines. `NAVIGATOR_DBOS_RECONCILE_INVOICES_ENABLED=true` selects DBOS and
//! requires its authenticated worker ingress; otherwise the trigger uses
//! Restate during the transition.
//!
//! Auth + env handling are identical to the canary trigger
//! (`src/bin/billing_canary_trigger.rs`): the shared trigger helpers attach only the
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
    let target = workflows::trigger::trigger_target(|name| std::env::var(name).ok())
        .context("selecting the ReconcileInvoices trigger ingress")?;
    let workflows::trigger::TriggerTarget { ingress, token } = target;

    workflows::trigger::start_dbos_reconcile(
        &ingress,
        &token,
        &serde_json::json!({
            "workflow_id": format!("ReconcileInvoices:{run_id}")
        }),
    )
    .await
    .context("triggering ReconcileInvoices through DBOS ingress")?;

    tracing::info!(%run_id, "reconcile-invoices workflow triggered");
    println!("triggered ReconcileInvoices/{run_id}");
    Ok(())
}
