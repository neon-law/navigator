//! Stable payloads for the Kubernetes jobs that submit DBOS workflows.

use chrono::{DateTime, TimeDelta, Timelike, Utc};
use serde_json::json;

/// Nightly Project Update reuses its workflow ID throughout each UTC day.
#[must_use]
pub fn nightly_project_update(now: DateTime<Utc>) -> serde_json::Value {
    let run_id = crate::nightly_project_update::run_key(now);
    json!({ "workflow_id": crate::dbos_nightly_project_update::workflow_id(&run_id) })
}

/// The scheduled video-comments poll. The `CronJob` runs every 15 minutes; a pod
/// restart in its retry window must join the same DBOS run even if the UTC
/// minute has advanced, so the DBOS tick is floored to the quarter hour.
#[must_use]
pub fn youtube_comments_poll(now: DateTime<Utc>) -> serde_json::Value {
    let slot = now - TimeDelta::minutes(i64::from(now.minute() % 15));
    json!({ "tick": slot.format("%Y-%m-%d-%H-%M").to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_nightly_project_update_names_one_run_per_utc_date() {
        let first = Utc.with_ymd_and_hms(2026, 11, 8, 3, 33, 0).unwrap();
        let retry = first + TimeDelta::minutes(7);
        let body = nightly_project_update(first);
        assert_eq!(body, nightly_project_update(retry));
        assert!(body["workflow_id"]
            .as_str()
            .unwrap()
            .ends_with("2026-11-08"));
    }

    #[test]
    fn the_youtube_dbos_tick_is_stable_across_cronjob_retries() {
        let first = Utc.with_ymd_and_hms(2026, 11, 8, 23, 45, 20).unwrap();
        let retry = Utc.with_ymd_and_hms(2026, 11, 8, 23, 48, 20).unwrap();
        let next = Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 20).unwrap();
        assert_eq!(
            youtube_comments_poll(first),
            json!({"tick": "2026-11-08-23-45"})
        );
        assert_eq!(youtube_comments_poll(retry), youtube_comments_poll(first));
        assert_eq!(
            youtube_comments_poll(next),
            json!({"tick": "2026-11-09-00-00"})
        );
    }
}
