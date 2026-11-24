//! Thin `CronJob` trigger for polling inbound `YouTube` comments into the
//! firm inbox.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-youtube-comments-poll-trigger",
        workflows_service::cron_triggers::youtube_comments_poll(chrono::Utc::now()),
    )
    .await
}
