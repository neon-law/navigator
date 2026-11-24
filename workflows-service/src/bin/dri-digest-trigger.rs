//! `dri-digest-trigger` — the thin `CronJob` entrypoint for the nightly
//! `DriDigest` workflow.
//!
//! Fires one `DriDigest` invocation against the Restate ingress, then exits.
//! The workflow key is the UTC run date, so a same-day re-fire is a no-op:
//! Restate admits at most one invocation per workflow key. The call is
//! one-way (`/send`): this process does no work beyond accepting the
//! invocation — Restate owns the retry schedule and runs the query → notify
//! steps on the `workflows-service` worker. Built from the shared
//! `images/Containerfile.trigger`
//! (`--build-arg CRATE=workflows-service --build-arg BIN=dri-digest-trigger`).
//!
//! Cadence: nightly at 01:11 UTC (the `dri-digest-trigger` `CronJob`
//! schedule) — off the top of the hour and clear of every other nightly
//! trigger (archives 10:00, reconcile-invoices, billing-digest 13:00, canary
//! Sunday 14:00), so a shared-resource hiccup on the hour never lines up with
//! this one.
//!
//! Auth: prod Restate Cloud authenticates every ingress call with the tenant
//! bearer (`RESTATE_AUTH_TOKEN`); the in-cluster KIND Operator does not. The
//! shared [`workflows::start_workflow`] helper attaches the header only when
//! the token is present and non-empty, so the same binary works in both.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    workflows::trigger::run_cron_trigger(
        "navigator-dri-digest-trigger",
        workflows_service::cron_triggers::dri_digest(chrono::Utc::now()),
    )
    .await
}
