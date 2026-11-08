//! Thin `CronJob` trigger for polling inbound `YouTube` comments into the
//! firm inbox.

use anyhow::{Context, Result};
use chrono::{DateTime, TimeDelta, Timelike, Utc};

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    let _telemetry = telemetry::init("navigator-youtube-comments-poll-trigger");
    let target = workflows::trigger::trigger_target(
        workflows::trigger::YOUTUBE_COMMENTS_DBOS_ENABLED_ENV,
        |name| std::env::var(name).ok(),
    )
    .context("selecting the YouTube comments trigger ingress")?;
    let now = Utc::now();
    let tick = match &target {
        workflows::trigger::TriggerTarget::Dbos { .. } => dbos_tick(now),
        workflows::trigger::TriggerTarget::Restate { .. } => {
            now.format("%Y-%m-%d-%H-%M").to_string()
        }
    };
    let request = workflows_service::youtube_comments::PollRequest { tick: tick.clone() };
    match target {
        workflows::trigger::TriggerTarget::Dbos { ingress, token } => {
            workflows::trigger::start_dbos_youtube_comments(&ingress, &token, &request).await
        }
        workflows::trigger::TriggerTarget::Restate {
            ingress,
            auth_token,
        } => workflows::start_workflow(
            &ingress,
            auth_token.as_deref(),
            "YoutubeCommentsPoller",
            &tick,
            "run",
            &request,
            true,
        )
        .await
        .map(|_| ()),
    }
    .context("triggering the scheduled YouTube comments poll")?;
    tracing::info!(workflow_service = "YoutubeCommentsPoller", %tick, "YouTube comments poll triggered");
    Ok(())
}

// The CronJob runs every 15 minutes. A pod restart in its retry window
// must join the same DBOS run even if the UTC minute has advanced.
fn dbos_tick(now: DateTime<Utc>) -> String {
    let slot = now - TimeDelta::minutes(i64::from(now.minute() % 15));
    slot.format("%Y-%m-%d-%H-%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::dbos_tick;
    use chrono::{TimeZone, Utc};

    #[test]
    fn dbos_tick_is_stable_across_cronjob_retries() {
        let first = Utc.with_ymd_and_hms(2026, 11, 8, 23, 45, 20).unwrap();
        let retry = Utc.with_ymd_and_hms(2026, 11, 8, 23, 48, 20).unwrap();
        let next = Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 20).unwrap();

        assert_eq!(dbos_tick(first), "2026-11-08-23-45");
        assert_eq!(dbos_tick(retry), dbos_tick(first));
        assert_eq!(dbos_tick(next), "2026-11-09-00-00");
    }
}
