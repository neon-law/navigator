//! Standalone durable workflow host.

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let telemetry = telemetry::init("navigator-workflows-service");
    telemetry::register_nightly_project_update();
    let result = Box::pin(workflows_service::host::run(shutdown_signal())).await;
    telemetry.shutdown();
    result
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.context("waiting for Ctrl-C"),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.context("waiting for Ctrl-C")
    }
}
