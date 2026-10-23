//! `Heartbeat`: the durable-execution liveness canary.
//!
//! It depends on nothing — no application table, no object storage, no third
//! party but the ops Slack webhook — so a posted line can only mean the engine
//! accepted a run, checkpointed a step, and ran a second step to completion.
//! `beat` checkpoints the instant the engine ran it, and `notify` posts that
//! checkpointed instant to the ops Slack channel. A recovery after `beat`
//! re-uses its checkpoint instead of reading the clock again. A six-hour
//! window with no line is the alarm.
//!
//! **The workflow id is the idempotency key.** DBOS 0.5.0 has no scheduler,
//! so [`schedule`] starts the workflow for each six-hour UTC slot with the id
//! [`workflow_id`] derives from that slot. A restart, or a second replica,
//! computes the same id, and a second start joins the first run.
//!
//! **`notify` is at least once.** Slack's incoming webhook takes no
//! idempotency key, so a crash between Slack's reply and the step's
//! checkpoint posts the line twice. A duplicate liveness line is harmless.

use std::sync::Arc;
use std::time::Duration;

use crate::Notifier;
use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};

use crate::heartbeat::{heartbeat_message, HeartbeatReport};

/// The registered workflow name, shared with the Restate service.
pub const WORKFLOW: &str = "Heartbeat";

/// The schedule's period: 00:00, 06:00, 12:00, and 18:00 UTC.
pub const PERIOD: Duration = Duration::from_hours(6);

/// How long the schedule waits before retrying a start that failed.
const RETRY_START: Duration = Duration::from_mins(1);

/// How many times `notify` posts before the workflow fails.
const NOTIFY_ATTEMPTS: u32 = 5;

/// The wall clock, injected so a test can count its reads.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// The registered heartbeat workflow.
pub type HeartbeatWorkflow = WorkflowRef<(), HeartbeatReport, HeartbeatError>;

/// The failure a heartbeat run records. It carries no provider detail.
#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum HeartbeatError {
    #[error("the ops notice was not delivered")]
    Notify,
}

/// The system clock.
#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// The six-hour UTC slot containing `now`.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    let period = i64::try_from(PERIOD.as_secs()).unwrap_or(i64::MAX);
    let seconds = now.timestamp();
    DateTime::from_timestamp(seconds - seconds.rem_euclid(period), 0).unwrap_or(now)
}

/// The workflow id for a slot: `Heartbeat-<UTC date>T<hour>`.
#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%dT%H"))
}

/// Register the heartbeat workflow. Call before [`DBOS::launch`].
///
/// # Errors
///
/// Fails when the instance is already launched or the name is taken.
pub fn register(
    dbos: &DBOS,
    notifier: Arc<dyn Notifier>,
    clock: Clock,
) -> dbos::Result<HeartbeatWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let notifier = Arc::clone(&notifier);
        let clock = Arc::clone(&clock);
        async move {
            let invocation_id = dbos::workflow_id().unwrap_or_default();
            let report = dbos::step("beat", || {
                let (invocation_id, clock) = (invocation_id.clone(), Arc::clone(&clock));
                async move {
                    dbos::Result::<_, HeartbeatError>::Ok(HeartbeatReport {
                        invocation_id,
                        beat_at: clock(),
                    })
                }
            })
            .await?;
            let message = heartbeat_message(&report);
            let options = StepOptions {
                max_attempts: NOTIFY_ATTEMPTS,
                ..StepOptions::default()
            };
            dbos::step_with("notify", options, || {
                let (notifier, message) = (Arc::clone(&notifier), message.clone());
                async move {
                    notifier
                        .notify(message)
                        .await
                        .map_err(|_| dbos::Error::Application(HeartbeatError::Notify))
                }
            })
            .await?;
            dbos::Result::<_, HeartbeatError>::Ok(report)
        }
    })
}

/// Start the heartbeat for `slot`. Starting a slot that already started
/// returns a handle to that run instead of a second run.
///
/// # Errors
///
/// Fails when the system database refuses the start.
pub async fn fire(
    workflow: &HeartbeatWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<HeartbeatReport, HeartbeatError>, HeartbeatError> {
    let id = workflow_id(slot);
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

/// Start the current slot's heartbeat, then each later slot's, until `stop`.
pub async fn schedule(workflow: HeartbeatWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = match fire(&workflow, current).await {
            Ok(_) => (current + PERIOD - now).to_std().unwrap_or(Duration::ZERO),
            Err(error) => {
                tracing::error!(
                    workflow_id = workflow_id(current),
                    %error,
                    "heartbeat start failed"
                );
                RETRY_START
            }
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{slot, workflow_id, PERIOD};
    use chrono::{TimeZone, Utc};

    #[test]
    fn slots_are_six_hour_utc_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 10, 13, 11, 59, 59).unwrap();
        assert_eq!(
            slot(now),
            Utc.with_ymd_and_hms(2026, 10, 13, 6, 0, 0).unwrap()
        );
        let boundary = Utc.with_ymd_and_hms(2026, 10, 13, 18, 0, 0).unwrap();
        assert_eq!(slot(boundary), boundary);
        assert_eq!(PERIOD.as_secs(), 6 * 60 * 60);
    }

    #[test]
    fn a_slot_names_one_workflow_id() {
        let now = Utc.with_ymd_and_hms(2026, 10, 13, 0, 30, 0).unwrap();
        assert_eq!(workflow_id(slot(now)), "Heartbeat-2026-10-13T00");
        let later = Utc.with_ymd_and_hms(2026, 10, 13, 5, 59, 0).unwrap();
        assert_eq!(workflow_id(slot(later)), workflow_id(slot(now)));
    }
}
