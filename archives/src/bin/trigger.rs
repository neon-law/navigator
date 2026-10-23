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

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    let _ = dotenvy::from_path(".devx/env");
    // One observability seam for every binary: stdout logs (JSON when an
    // OTLP endpoint is set) plus OTLP traces + metrics. Held to end of main
    // so the drop flushes any batched export before the process exits.
    let _telemetry = telemetry::init("navigator-archives-trigger");

    let ingress = std::env::var("RESTATE_INGRESS_URL")
        .context("RESTATE_INGRESS_URL must be set (the Restate ingress endpoint)")?;
    // Optional bearer — present only when targeting Restate Cloud.
    let auth_token = std::env::var("RESTATE_AUTH_TOKEN").ok();
    // Each bounded trigger retry resolves to the same UTC scheduled time.
    let requested = std::env::var("ARCHIVES_SCHEDULED_FOR").ok();
    let now = requested.as_deref().map_or_else(
        || Ok(chrono::Utc::now()),
        |value| {
            value
                .parse::<chrono::DateTime<chrono::Utc>>()
                .context("parse ARCHIVES_SCHEDULED_FOR as RFC 3339")
        },
    )?;
    let scheduled_for = scheduled_run(now);
    anyhow::ensure!(
        requested.is_none() || scheduled_for == now,
        "ARCHIVES_SCHEDULED_FOR must name 05:55 or 17:55 UTC exactly"
    );
    let run_id = scheduled_for.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let _response = workflows::start_workflow(
        &ingress,
        auth_token.as_deref(),
        "Archives",
        &run_id,
        "run",
        &serde_json::json!({ "scheduled_for": scheduled_for }),
        true, // one-way: accept the invocation and exit; Restate runs it.
    )
    .await
    .context("triggering Archives workflow")?;

    tracing::info!(%run_id, "archives workflow triggered");
    println!("triggered Archives/{run_id}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::scheduled_run;
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
