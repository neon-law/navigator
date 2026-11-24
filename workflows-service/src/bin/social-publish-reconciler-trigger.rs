//! Thin `CronJob` trigger for recovering scheduled Blog Post submissions.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-social-publish-reconciler-trigger",
        workflows_service::cron_triggers::social_publish_reconciler(chrono::Utc::now()),
    )
    .await
}
