//! Thin daily trigger for the Nightly Project Update.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-nightly-project-update-trigger",
        workflows_service::cron_triggers::nightly_project_update(chrono::Utc::now()),
    )
    .await
}
