//! `archives trigger` — the thin `CronJob` entrypoint.
//!
//! Fires one `Archives` workflow invocation against the Restate
//! ingress, then exits. The workflow key is the UTC scheduled time, so
//! retries within either daily run are idempotent: Restate runs a given key
//! at most once. The call is one-way (`/send`): this process does no
//! work beyond accepting the invocation — Restate owns the retry
//! schedule and runs the snapshot → cost → email steps on the
//! `workflows-service` worker (the `email` step now posts a Slack digest).
//!
//! Auth: prod Restate Cloud authenticates every ingress call with the
//! tenant bearer (`RESTATE_AUTH_TOKEN`); the in-cluster KIND Operator
//! does not. The shared [`workflows::start_workflow`] helper attaches
//! the header only when the token is present and non-empty, so the
//! same binary works in both environments.

use anyhow::{Context, Result};

fn scheduled_run(now: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::Utc> {
    let morning = now
        .date_naive()
        .and_hms_opt(5, 55, 0)
        .expect("05:55:00 is a valid time")
        .and_utc();
    let evening = morning + chrono::Duration::hours(12);
    if now >= evening {
        evening
    } else if now >= morning {
        morning
    } else {
        evening - chrono::Duration::days(1)
    }
}

/// The Restate key and body for the run `now` belongs to. Each bounded
/// trigger retry resolves to the same UTC scheduled time.
fn restate_run(
    lookup: &dyn Fn(&str) -> Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(String, serde_json::Value)> {
    anyhow::ensure!(
        lookup("NAVIGATOR_ARCHIVE_WRITE_ONLY_CONFIG").is_none(),
        "write-only archive scheduling requires selected DBOS"
    );
    let requested = lookup("ARCHIVES_SCHEDULED_FOR");
    let now = requested.as_deref().map_or(Ok(now), |value| {
        value
            .parse::<chrono::DateTime<chrono::Utc>>()
            .context("parse ARCHIVES_SCHEDULED_FOR as RFC 3339")
    })?;
    let scheduled_for = scheduled_run(now);
    anyhow::ensure!(
        requested.is_none() || scheduled_for == now,
        "ARCHIVES_SCHEDULED_FOR must name 05:55 or 17:55 UTC exactly"
    );
    Ok((
        scheduled_for.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        serde_json::json!({ "scheduled_for": scheduled_for }),
    ))
}

#[tokio::main]
async fn main() -> Result<()> {
    let now = chrono::Utc::now();
    workflows::trigger::run_cron_trigger(
        "navigator-archives-trigger",
        workflows::trigger::CronTrigger {
            flag: "NAVIGATOR_DBOS_ARCHIVES_ENABLED",
            service: "Archives",
            dbos: None,
            restate: Box::new(move |lookup| restate_run(lookup, now)),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::{restate_run, scheduled_run};
    use chrono::{DateTime, Utc};

    fn at(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    #[test]
    fn morning_and_evening_are_distinct_and_retries_attach_to_the_same_run() {
        let morning = scheduled_run(at("2026-10-11T05:55:00Z"));
        let evening = scheduled_run(at("2026-10-11T17:55:00Z"));
        assert_eq!(morning.to_rfc3339(), "2026-10-11T05:55:00+00:00");
        assert_eq!(evening.to_rfc3339(), "2026-10-11T17:55:00+00:00");
        assert_ne!(morning, evening);
        assert_eq!(morning, scheduled_run(at("2026-10-11T05:56:59Z")));
        assert_eq!(evening, scheduled_run(at("2026-10-11T17:56:59Z")));
    }

    #[test]
    fn before_first_daily_fire_uses_the_previous_evening() {
        assert_eq!(
            scheduled_run(at("2026-10-11T05:54:59Z")),
            at("2026-10-10T17:55:00Z")
        );
        assert_eq!(
            scheduled_run(at("2026-10-11T17:54:59Z")),
            at("2026-10-11T05:55:00Z")
        );
    }

    #[test]
    fn the_restate_run_is_keyed_on_the_scheduled_slot_and_carries_it() {
        let (key, body) = restate_run(&|_| None, at("2026-10-11T06:01:30Z")).unwrap();
        assert_eq!(key, "2026-10-11T05:55:00Z");
        assert_eq!(
            body,
            serde_json::json!({"scheduled_for": "2026-10-11T05:55:00Z"})
        );
        // A requested slot replaces the clock.
        let requested = |name: &str| {
            (name == "ARCHIVES_SCHEDULED_FOR").then(|| "2026-10-10T17:55:00Z".to_owned())
        };
        let (key, _) = restate_run(&requested, at("2026-10-11T06:01:30Z")).unwrap();
        assert_eq!(key, "2026-10-10T17:55:00Z");
    }

    #[test]
    fn restate_refuses_write_only_archives_and_a_requested_time_off_the_slot() {
        for (name, value, message) in [
            (
                "NAVIGATOR_ARCHIVE_WRITE_ONLY_CONFIG",
                "/config/archive.json",
                "write-only archive scheduling requires selected DBOS",
            ),
            (
                "ARCHIVES_SCHEDULED_FOR",
                "noon",
                "parse ARCHIVES_SCHEDULED_FOR as RFC 3339",
            ),
            (
                "ARCHIVES_SCHEDULED_FOR",
                "2026-10-11T05:56:00Z",
                "ARCHIVES_SCHEDULED_FOR must name 05:55 or 17:55 UTC exactly",
            ),
        ] {
            let lookup = |asked: &str| (asked == name).then(|| value.to_owned());
            let error = restate_run(&lookup, at("2026-10-11T06:00:00Z")).unwrap_err();
            assert_eq!(error.to_string(), message);
        }
    }

    #[test]
    fn cloud_and_kind_both_fire_at_0555_and_1755_utc() {
        for manifest in [
            include_str!("../../../examples/deploy/k8s/exports/cron-archives-trigger.yaml"),
            include_str!("../../../k8s/overlays/kind/archives-trigger/archives-trigger.yaml"),
        ] {
            assert!(manifest.contains("schedule: \"55 5,17 * * *\""));
            assert!(manifest.contains("timeZone: Etc/UTC"));
        }
    }
}
