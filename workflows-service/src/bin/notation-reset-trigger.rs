//! `notation-reset-trigger` — the thin `CronJob` entrypoint for the daily
//! `NotationReset` workflow.
//!
//! Fires one `NotationReset` invocation against the Restate ingress, then
//! exits. The workflow key is the UTC run date, so a same-day re-fire is a
//! no-op: Restate admits at most one invocation per workflow key. The call
//! is one-way (`/send`): Restate owns the retry schedule and runs the
//! discover → delete → post steps on the `workflows-service` worker. Built
//! from the shared `images/Containerfile.trigger`
//! (`--build-arg CRATE=workflows-service --build-arg BIN=notation-reset-trigger`).
//!
//! Cadence: daily at 04:20 UTC (the `notation-reset-trigger` `CronJob`
//! schedule), clear of every other nightly trigger.
//!
//! Auth: prod Restate Cloud authenticates every ingress call with the tenant
//! bearer (`RESTATE_AUTH_TOKEN`); the in-cluster KIND Operator does not. The
//! shared [`workflows::start_workflow`] helper attaches the header only when
//! the token is present and non-empty, so the same binary works in both.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-notation-reset-trigger",
        workflows_service::cron_triggers::notation_reset(chrono::Utc::now()),
    )
    .await
}
