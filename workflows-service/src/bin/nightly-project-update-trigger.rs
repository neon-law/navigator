//! Thin daily trigger for the Nightly Project Update.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_dbos_cron(
        "navigator-nightly-project-update-trigger",
        "NightlyProjectUpdate",
        "/internal/dbos/nightly-project-update",
        &workflows_service::cron_triggers::nightly_project_update(chrono::Utc::now()),
    )
    .await
}
