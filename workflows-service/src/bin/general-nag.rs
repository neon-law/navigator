//! `general-nag` — the thin `CronJob` entrypoint for the daily `GeneralNag`
//! workflow.
//!
//! Fires one `GeneralNag` invocation against the Restate ingress, then exits.
//! The workflow key is the UTC run date, so a same-day re-fire is a no-op:
//! Restate admits at most one invocation per workflow key. The call is
//! one-way (`/send`): this process does no work beyond accepting the
//! invocation — Restate owns the retry schedule and runs the notify →
//! (optional) counts steps on the `workflows-service` worker. Built from the
//! shared `images/Containerfile.trigger`
//! (`--build-arg CRATE=workflows-service --build-arg BIN=general-nag`).
//!
//! Cadence: daily at 01:11 UTC (the `general-nag` `CronJob` schedule) —
//! the same minute as `dri-digest-trigger`. Both are thin, sub-second calls
//! to the Restate ingress for different workflows, so the shared minute
//! costs nothing beyond the coincidence.
//!
//! Auth: prod Restate Cloud authenticates every ingress call with the tenant
//! bearer (`RESTATE_AUTH_TOKEN`); the in-cluster KIND Operator does not. The
//! shared [`workflows::start_workflow`] helper attaches the header only when
//! the token is present and non-empty, so the same binary works in both.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-general-nag",
        workflows_service::cron_triggers::general_nag(chrono::Utc::now()),
    )
    .await
}
