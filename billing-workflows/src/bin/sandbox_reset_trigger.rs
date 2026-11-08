//! `sandbox-credit-reset trigger` — the thin monthly `CronJob` entrypoint.
//!
//! Submits one `SandboxCreditReset` run to the selected durable engine.
//! The UTC billing month names the run. A same-month re-fire joins that run,
//! and the next month's fire starts a new one.

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
    let target = workflows::trigger::trigger_target(
        workflows::trigger::SANDBOX_CREDIT_RESET_DBOS_ENABLED_ENV,
        |name| std::env::var(name).ok(),
    )
    .context("selecting the SandboxCreditReset trigger ingress")?;
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_sandbox_credit_reset(&ingress, &token, &run_id)
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

    // run_id (a billing month, "YYYY-MM") intentionally stays out of the
    // log lines: CodeQL's rust/cleartext-logging query flags any period
    // value reaching a log sink, and the observability contract does not
    // require it — Restate's own invocation log carries the keyed run id.
    tracing::info!("sandbox-credit-reset workflow triggered");
    println!("triggered SandboxCreditReset");
    Ok(())
}
