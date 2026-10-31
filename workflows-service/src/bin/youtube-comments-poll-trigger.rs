//! Thin `CronJob` trigger for polling inbound `YouTube` comments into the
//! firm inbox.

use anyhow::{Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-youtube-comments-poll-trigger");
    let ingress =
        std::env::var("RESTATE_INGRESS_URL").context("RESTATE_INGRESS_URL must be set")?;
    let auth = std::env::var("RESTATE_AUTH_TOKEN").ok();
    let tick = chrono::Utc::now().format("%Y-%m-%d-%H-%M").to_string();
    workflows::start_workflow(
        &ingress,
        auth.as_deref(),
        "YoutubeCommentsPoller",
        &tick,
        "run",
        &workflows_service::youtube_comments::PollRequest { tick: tick.clone() },
        true,
    )
    .await
    .context("triggering the scheduled YouTube comments poll")?;
    tracing::info!(workflow_service = "YoutubeCommentsPoller", %tick, "YouTube comments poll triggered");
    Ok(())
}
