//! `notation-reset-trigger` — the thin `CronJob` entrypoint for the daily
//! `NotationReset` workflow.
//!
//! Fires one `NotationReset` invocation against the Restate ingress, then
//! exits. The workflow key is the UTC run date, so a same-day re-fire is a
//! no-op: Restate admits at most one invocation per workflow key. The call
//! is one-way (`/send`): Restate owns the retry schedule and runs the
//! discover → delete → post steps on the `workflows-service` worker. Built
//! from the shared `images/Containerfile.trigger`
//! (`--build-arg CRATE=workflows-service --build-arg BIN=notation-reset-trigger`).
//!
//! Cadence: daily at 04:20 UTC (the `notation-reset-trigger` `CronJob`
//! schedule), clear of every other nightly trigger.
//!
//! Auth: prod Restate Cloud authenticates every ingress call with the tenant
//! bearer (`RESTATE_AUTH_TOKEN`); the in-cluster KIND Operator does not. The
//! shared [`workflows::start_workflow`] helper attaches the header only when
//! the token is present and non-empty, so the same binary works in both.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-notation-reset-trigger");

    let ingress = std::env::var("RESTATE_INGRESS_URL")
        .context("RESTATE_INGRESS_URL must be set (the Restate ingress endpoint)")?;
    let auth_token = std::env::var("RESTATE_AUTH_TOKEN").ok();
    let run_id = workflows_service::notation_reset::run_key(chrono::Utc::now());
    let _response = workflows::start_workflow(
        &ingress,
        auth_token.as_deref(),
        "NotationReset",
        &run_id,
        "run",
        &serde_json::json!({}),
        true,
    )
    .await
    .context("triggering NotationReset workflow")?;

    tracing::info!(%run_id, "notation reset triggered");
    println!("triggered NotationReset/{run_id}");
    Ok(())
}
