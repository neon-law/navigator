//! DBOS port of the daily `GeneralNag` operations notice.
//!
//! Runs once per UTC date when explicitly enabled. Its workflow id is stable
//! across replicas and restarts; Slack itself provides no idempotency key, so
//! a crash after Slack accepts a post but before its DBOS checkpoint can still
//! produce one duplicate delivery.

use std::sync::Arc;
use std::time::Duration;

use crate::SlackBot;
use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};

pub const WORKFLOW: &str = "GeneralNag";
/// Separate from the general DBOS executor switch so a staged executor can
/// coexist with the live Restate CronJob until cutover is explicitly chosen.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_GENERAL_NAG_ENABLED";
const RETRY_START: Duration = Duration::from_mins(1);
const NOTIFY_ATTEMPTS: u32 = 5;

/// Whether this one workflow is enabled in the DBOS host. Disabled unless
/// explicitly set, so enabling the executor alone cannot duplicate the live
/// Restate CronJob.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, super::SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(super::SettingsError::Invalid(ENABLED_ENV)),
    }
}

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type GeneralNagWorkflow = WorkflowRef<(), GeneralNagReport, GeneralNagError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralNagReport {
    pub posted_followup: bool,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum GeneralNagError {
    #[error("the general notice was not delivered")]
    Notify,
    #[error("the open-matters count could not be read")]
    Counts,
}

#[async_trait::async_trait]
pub trait CountSource: Send + Sync {
    async fn read(&self) -> Result<(usize, usize), GeneralNagError>;
}

#[derive(Clone)]
pub struct Context {
    pub slack: Arc<dyn SlackBot>,
    pub channel_id: String,
    pub simulated: bool,
    pub counts: Arc<dyn CountSource>,
}

struct SurrealCounts(store::surreal::SurrealDb);

#[async_trait::async_trait]
impl CountSource for SurrealCounts {
    async fn read(&self) -> Result<(usize, usize), GeneralNagError> {
        store::projects::matter_open_pitch_counts(&self.0)
            .await
            .map_err(|_| GeneralNagError::Counts)
    }
}

impl Context {
    /// Resolve the live bot and target only when the GeneralNag schedule is
    /// enabled. A capture-only backend must never count as a successful
    /// production delivery.
    pub fn from_lookup<F>(
        surreal: store::surreal::SurrealDb,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let channel_id = lookup("SLACK_GENERAL_CHANNEL_ID")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(super::SettingsError::Missing("SLACK_GENERAL_CHANNEL_ID"))?;
        let token = lookup("SLACK_BOT_TOKEN")
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(super::SettingsError::Missing("SLACK_BOT_TOKEN"))?;
        let base_url = crate::harness_override(&lookup, "SLACK_API_BASE_URL")
            .map_err(|_| super::SettingsError::Invalid("SLACK_API_BASE_URL"))?;
        let slack: Arc<dyn SlackBot> = match base_url {
            Some(base_url) => Arc::new(crate::SlackBotClient::with_base_url(token, base_url)),
            None => Arc::new(crate::SlackBotClient::new(token)),
        };
        Ok(Self {
            slack,
            channel_id,
            simulated,
            counts: Arc::new(SurrealCounts(surreal)),
        })
    }
}

pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(1, 11, 0)
        .and_then(|time| time.and_local_timezone(Utc).single())
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%d"))
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<GeneralNagWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            if context.channel_id.is_empty() {
                return Err(dbos::Error::Application(GeneralNagError::Notify));
            }
            let channel_id = context.channel_id.clone();
            let slack = Arc::clone(&context.slack);
            dbos::step_with(
                "notify",
                StepOptions {
                    max_attempts: NOTIFY_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let (channel_id, slack) = (channel_id.clone(), Arc::clone(&slack));
                    async move {
                        slack
                            .post_message(&channel_id, "Nobody Cares, Work Harder")
                            .await
                            .map_err(|_| dbos::Error::Application(GeneralNagError::Notify))
                    }
                },
            )
            .await?;

            let mut posted_followup = false;
            if !context.simulated {
                let counts = Arc::clone(&context.counts);
                let (open, pitches) = dbos::step("counts", move || {
                    let counts = Arc::clone(&counts);
                    async move { counts.read().await.map_err(dbos::Error::Application) }
                })
                .await?;
                let followup = format!("Total open matters: {open}, pitches: {pitches}");
                let channel_id = context.channel_id.clone();
                let slack = Arc::clone(&context.slack);
                dbos::step_with(
                    "notify_counts",
                    StepOptions {
                        max_attempts: NOTIFY_ATTEMPTS,
                        ..StepOptions::default()
                    },
                    move || {
                        let (channel_id, followup, slack) =
                            (channel_id.clone(), followup.clone(), Arc::clone(&slack));
                        async move {
                            slack
                                .post_message(&channel_id, &followup)
                                .await
                                .map_err(|_| dbos::Error::Application(GeneralNagError::Notify))
                        }
                    },
                )
                .await?;
                posted_followup = true;
            }
            dbos::Result::<_, GeneralNagError>::Ok(GeneralNagReport { posted_followup })
        }
    })
}

pub async fn fire(
    workflow: &GeneralNagWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<GeneralNagReport, GeneralNagError>, GeneralNagError> {
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

pub async fn schedule(workflow: GeneralNagWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = if now >= current {
            match fire(&workflow, current).await {
                Ok(_) => (current + Duration::from_hours(24) - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(current), %error, "general nag start failed");
                    RETRY_START
                }
            }
        } else {
            (current - now).to_std().unwrap_or(Duration::ZERO)
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{enabled, slot, workflow_id, ENABLED_ENV};
    use chrono::{TimeZone, Utc};
    use std::sync::Arc;

    struct EmptyCounts;

    #[async_trait::async_trait]
    impl super::CountSource for EmptyCounts {
        async fn read(&self) -> Result<(usize, usize), super::GeneralNagError> {
            Ok((0, 0))
        }
    }

    fn unlaunched_workflow() -> super::GeneralNagWorkflow {
        let dbos = dbos::DBOS::new(crate::dbos_engine::config(
            "postgres://localhost/navigator?sslmode=disable",
            "test",
        ));
        super::register(
            &dbos,
            super::Context {
                slack: Arc::new(crate::notify::CapturingSlackBot::new()),
                channel_id: "C_GENERAL".to_owned(),
                simulated: true,
                counts: Arc::new(EmptyCounts),
            },
        )
        .unwrap()
    }

    #[test]
    fn every_instant_on_a_utc_date_uses_the_same_daily_slot() {
        let early = Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 10, 13, 23, 59, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 13, 1, 11, 0).unwrap();
        assert_eq!(slot(early), expected);
        assert_eq!(slot(late), expected);
        assert_eq!(workflow_id(slot(early)), "GeneralNag-2026-10-13");
    }

    #[test]
    fn schedule_requires_an_explicit_true_flag() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some("false".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }

    #[test]
    fn enabled_schedule_rejects_capture_only_configuration() {
        let result =
            super::Context::from_lookup(store::surreal::test_support::unreachable(), true, |_| {
                None
            });
        assert!(matches!(
            result,
            Err(super::super::SettingsError::Missing(
                "SLACK_GENERAL_CHANNEL_ID"
            ))
        ));

        let result =
            super::Context::from_lookup(store::surreal::test_support::unreachable(), true, |key| {
                (key == "SLACK_GENERAL_CHANNEL_ID").then(|| "C_GENERAL".to_owned())
            });
        assert!(matches!(
            result,
            Err(super::super::SettingsError::Missing("SLACK_BOT_TOKEN"))
        ));

        let result =
            super::Context::from_lookup(store::surreal::test_support::unreachable(), true, |key| {
                match key {
                    "SLACK_GENERAL_CHANNEL_ID" => Some("  ".to_owned()),
                    "SLACK_BOT_TOKEN" => Some("xoxb-test".to_owned()),
                    _ => None,
                }
            });
        assert!(matches!(
            result,
            Err(super::super::SettingsError::Missing(
                "SLACK_GENERAL_CHANNEL_ID"
            ))
        ));
    }

    #[test]
    fn api_endpoint_override_requires_the_ci_harness() {
        let configured = |harness: bool| {
            super::Context::from_lookup(store::surreal::test_support::unreachable(), true, |key| {
                match key {
                    "SLACK_GENERAL_CHANNEL_ID" => Some("C_GENERAL".to_owned()),
                    "SLACK_BOT_TOKEN" => Some("xoxb-test".to_owned()),
                    "SLACK_API_BASE_URL" => Some("http://127.0.0.1:3000".to_owned()),
                    "NAVIGATOR_CI_HARNESS" if harness => Some("1".to_owned()),
                    _ => None,
                }
            })
        };
        assert!(configured(true).is_ok());
        assert!(matches!(
            configured(false),
            Err(super::super::SettingsError::Invalid("SLACK_API_BASE_URL"))
        ));
    }

    #[tokio::test]
    async fn schedule_waiting_before_the_slot_exits_when_cancelled() {
        let workflow = unlaunched_workflow();
        let now = Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap();
        let clock: super::Clock = Arc::new(move || now);
        let stop = dbos::CancellationToken::new();
        stop.cancel();

        super::schedule(workflow, clock, stop).await;
    }

    #[tokio::test]
    async fn schedule_cancels_after_a_failed_start_attempt() {
        let workflow = unlaunched_workflow();
        let now = Utc.with_ymd_and_hms(2026, 10, 13, 1, 11, 0).unwrap();
        let clock: super::Clock = Arc::new(move || now);
        let stop = dbos::CancellationToken::new();
        stop.cancel();

        super::schedule(workflow, clock, stop).await;
    }
}
