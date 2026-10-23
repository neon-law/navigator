//! DBOS port of the daily `NotationReset`: delete every Notation draft and
//! stored preview draft on the deployment, then post one message to the
//! internal messages channel.
//!
//! The workflow id is `NotationReset-<UTC date>`, the same day key the Restate
//! trigger uses. Steps are `discover`, one `delete-notation-<id>` or
//! `delete-preview-<id>` per draft, then `post`. A delete repeated by recovery
//! is safe without an effect key: it re-reads the row and reports
//! `AlreadyGone` or deletes nothing. Slack's `chat.postMessage` takes no
//! idempotency key, so a crash after Slack accepts the post and before its
//! checkpoint posts the message twice.
//!
//! The schedule starts today's 04:20 UTC slot only within
//! [`LATE_START_WINDOW`] of it ([`daily::LateStart::Within`]). An executor
//! started later waits for tomorrow, so switching engines mid-day never
//! repeats the Restate run and never deletes drafts made since 04:20.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::notation_reset::ResetOutcome;
use store::surreal::SurrealDb;
use uuid::Uuid;

use super::daily::{self, LateStart};
use crate::notation_reset::{
    discover_drafts, elapsed_ms, render_failure, render_report, DraftRoster, NotationResetReport,
};
use crate::SlackBot;

pub const WORKFLOW: &str = "NotationReset";
/// Independent opt-in keeps the Restate CronJob as the active schedule until
/// the deployment explicitly switches engines.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_NOTATION_RESET_ENABLED";
/// How late after 04:20 UTC an executor may still start today's reset.
pub const LATE_START_WINDOW: Duration = Duration::from_mins(15);
const ATTEMPTS: u32 = 3;
const POST_ATTEMPTS: u32 = 5;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type NotationResetWorkflow = WorkflowRef<(), NotationResetReport, NotationResetError>;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum NotationResetError {
    #[error("the Notation drafts could not be discovered")]
    Discover,
    #[error("a Notation draft could not be deleted")]
    Delete,
    #[error("the notation reset message was not delivered")]
    Post,
}

/// Whether this schedule is enabled. Only literal `true` opts in.
///
/// # Errors
///
/// Refuses any value other than `true` or `false`.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, super::SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(super::SettingsError::Invalid(ENABLED_ENV)),
    }
}

#[derive(Clone)]
pub struct Context {
    pub surreal: SurrealDb,
    pub slack: Arc<dyn SlackBot>,
    pub channel_id: String,
    pub environment: &'static str,
}

impl Context {
    /// Resolve the live bot and the messages channel this deployment posts to.
    /// A simulated-matters deployment posts to
    /// `SLACK_STAGING_MESSAGES_CHANNEL_ID`, any other to
    /// `SLACK_MESSAGES_CHANNEL_ID`.
    ///
    /// # Errors
    ///
    /// Fails when the channel or `SLACK_BOT_TOKEN` is unset or blank, or a
    /// Slack API override is set outside the CI harness.
    pub fn from_lookup<F>(
        surreal: SurrealDb,
        environment: &'static str,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let required = |key: &'static str| {
            lookup(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .ok_or(super::SettingsError::Missing(key))
        };
        let channel_id = required(if simulated {
            "SLACK_STAGING_MESSAGES_CHANNEL_ID"
        } else {
            "SLACK_MESSAGES_CHANNEL_ID"
        })?;
        let token = required("SLACK_BOT_TOKEN")?;
        let base_url = crate::harness_override(&lookup, "SLACK_API_BASE_URL")
            .map_err(|_| super::SettingsError::Invalid("SLACK_API_BASE_URL"))?;
        let slack: Arc<dyn SlackBot> = match base_url {
            Some(base_url) => Arc::new(crate::SlackBotClient::with_base_url(token, base_url)),
            None => Arc::new(crate::SlackBotClient::new(token)),
        };
        Ok(Self {
            surreal,
            slack,
            channel_id,
            environment,
        })
    }
}

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// 04:20 UTC, the Restate CronJob's time.
#[must_use]
pub fn slot_time() -> NaiveTime {
    NaiveTime::from_hms_opt(4, 20, 0).unwrap_or(NaiveTime::MIN)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", crate::notation_reset::run_key(slot))
}

fn options(max_attempts: u32) -> StepOptions<NotationResetError> {
    StepOptions {
        max_attempts,
        ..StepOptions::default()
    }
}

async fn now_step(name: &str) -> dbos::Result<DateTime<Utc>, NotationResetError> {
    dbos::step(name, || async {
        Ok::<_, dbos::Error<NotationResetError>>(Utc::now())
    })
    .await
}

async fn post(context: &Context, message: String) -> dbos::Result<(), NotationResetError> {
    let (slack, channel_id) = (Arc::clone(&context.slack), context.channel_id.clone());
    dbos::step_with("post", options(POST_ATTEMPTS), move || {
        let (slack, channel_id, message) =
            (Arc::clone(&slack), channel_id.clone(), message.clone());
        async move {
            slack
                .post_message(&channel_id, &message)
                .await
                .map_err(|_| dbos::Error::Application(NotationResetError::Post))
        }
    })
    .await
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<NotationResetWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let started = now_step("started").await?;
            let surreal = context.surreal.clone();
            let roster = dbos::step_with("discover", options(ATTEMPTS), move || {
                let surreal = surreal.clone();
                async move {
                    discover_drafts(&surreal)
                        .await
                        .map_err(|_| dbos::Error::Application(NotationResetError::Discover))
                }
            })
            .await;
            let roster: DraftRoster = match roster {
                Ok(roster) => roster,
                Err(error) => {
                    let finished = now_step("finished").await?;
                    let message =
                        render_failure(context.environment, elapsed_ms(started, finished));
                    post(&context, message).await?;
                    return Err(error);
                }
            };

            let mut report = NotationResetReport::default();
            for id in roster.notations {
                match delete(&context.surreal, "notation", id).await {
                    Ok(Some(outcome)) => report.absorb(outcome),
                    Ok(None) => {}
                    Err(_) => report.failures.push(id),
                }
            }
            for id in roster.previews {
                match delete(&context.surreal, "preview", id).await {
                    Ok(_) => report.previews_deleted += 1,
                    Err(_) => report.failures.push(id),
                }
            }

            let finished = now_step("finished").await?;
            report.duration_ms = elapsed_ms(started, finished);
            post(&context, render_report(context.environment, &report)).await?;
            tracing::info!(
                notations_deleted = report.notations_deleted,
                previews_deleted = report.previews_deleted,
                kept = report.kept,
                failures = report.failures.len(),
                duration_ms = report.duration_ms,
                "notation reset complete"
            );
            dbos::Result::<_, NotationResetError>::Ok(report)
        }
    })
}

/// One checkpointed delete. A Notation reports its outcome; a preview
/// reports `None`.
async fn delete(
    surreal: &SurrealDb,
    kind: &'static str,
    id: Uuid,
) -> dbos::Result<Option<ResetOutcome>, NotationResetError> {
    let surreal = surreal.clone();
    dbos::step_with(
        &format!("delete-{kind}-{id}"),
        options(ATTEMPTS),
        move || {
            let surreal = surreal.clone();
            async move {
                let deleted = if kind == "notation" {
                    store::notation_reset::delete_draft(&surreal, id)
                        .await
                        .map(Some)
                } else {
                    store::notation_reset::delete_preview_draft(&surreal, id)
                        .await
                        .map(|()| None)
                };
                deleted.map_err(|error| {
                    tracing::warn!(draft_id = %id, kind, %error, "notation reset delete failed");
                    dbos::Error::Application(NotationResetError::Delete)
                })
            }
        },
    )
    .await
}

pub async fn fire(
    workflow: &NotationResetWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<NotationResetReport, NotationResetError>, NotationResetError> {
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

pub async fn schedule(workflow: NotationResetWorkflow, clock: Clock, stop: CancellationToken) {
    daily::run(
        WORKFLOW,
        slot_time(),
        LateStart::Within(LATE_START_WINDOW),
        move || clock(),
        stop,
        |slot| {
            let workflow = &workflow;
            async move { fire(workflow, slot).await.map(|_| ()) }
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::{enabled, slot_time, workflow_id, Context, ENABLED_ENV};
    use crate::dbos_engine::daily::{next_slot, LateStart};
    use chrono::{TimeZone, Utc};

    #[test]
    fn the_workflow_id_is_the_restate_day_key() {
        let at = Utc.with_ymd_and_hms(2026, 10, 21, 4, 20, 0).unwrap();
        assert_eq!(workflow_id(at), "NotationReset-2026-10-21");
    }

    #[test]
    fn a_mid_day_start_waits_for_tomorrow() {
        let late = LateStart::Within(super::LATE_START_WINDOW);
        let noon = Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap();
        assert_eq!(
            next_slot(noon, slot_time(), late),
            Utc.with_ymd_and_hms(2026, 10, 22, 4, 20, 0).unwrap()
        );
        let rolling = Utc.with_ymd_and_hms(2026, 10, 21, 4, 27, 0).unwrap();
        assert_eq!(
            next_slot(rolling, slot_time(), late),
            Utc.with_ymd_and_hms(2026, 10, 21, 4, 20, 0).unwrap()
        );
    }

    #[test]
    fn schedule_requires_a_literal_true_flag() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some("false".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }

    #[test]
    fn an_enabled_schedule_requires_its_channel_and_bot() {
        let configured = |simulated: bool, pairs: &'static [(&'static str, &'static str)]| {
            Context::from_lookup(
                store::surreal::test_support::unreachable(),
                "staging",
                simulated,
                move |key| {
                    pairs
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| (*value).to_owned())
                },
            )
            .map(|context| context.channel_id)
        };
        assert_eq!(
            configured(true, &[("SLACK_BOT_TOKEN", "xoxb-test")]).unwrap_err(),
            super::super::SettingsError::Missing("SLACK_STAGING_MESSAGES_CHANNEL_ID")
        );
        assert_eq!(
            configured(false, &[("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C-STAGING")]).unwrap_err(),
            super::super::SettingsError::Missing("SLACK_MESSAGES_CHANNEL_ID")
        );
        assert_eq!(
            configured(true, &[("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C-STAGING")]).unwrap_err(),
            super::super::SettingsError::Missing("SLACK_BOT_TOKEN")
        );
        assert_eq!(
            configured(
                true,
                &[
                    ("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C-STAGING"),
                    ("SLACK_MESSAGES_CHANNEL_ID", "C-MESSAGES"),
                    ("SLACK_BOT_TOKEN", "xoxb-test"),
                ]
            )
            .unwrap(),
            "C-STAGING"
        );
    }
}
