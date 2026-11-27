//! DBOS port of the daily `GeneralNag` operations notice.
//!
//! Runs once per UTC date when explicitly enabled. Its workflow id is stable
//! across replicas and restarts. Each post uses its Effect Key as a receipt
//! correlation. Recovery searches retained history from the original slot's UTC
//! date, reuses a found receipt, and refuses incomplete history before sending.

use std::sync::Arc;
use std::time::Duration;

use crate::dri_digest::{open_matters_message, slack_firm_open_pitch_counts};
use crate::firm_slack::{DeploymentSlackFirm, SLACK_FIRM_ENV};
use crate::SlackBot;
use chrono::{DateTime, NaiveDate, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::projects::BrandOpenPitch;

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
    #[error("the general notice delivery could not be confirmed")]
    Notify,
    #[error("the open-matters count could not be read")]
    Counts,
}

#[async_trait::async_trait]
pub trait CountSource: Send + Sync {
    async fn read(&self) -> Result<Vec<BrandOpenPitch>, GeneralNagError>;
}

#[derive(Clone)]
pub struct Context {
    pub slack: Arc<dyn SlackBot>,
    pub channel_id: String,
    pub simulated: bool,
    pub counts: Arc<dyn CountSource>,
}

struct SurrealCounts(store::surreal::SurrealDb, DeploymentSlackFirm);

#[async_trait::async_trait]
impl CountSource for SurrealCounts {
    async fn read(&self) -> Result<Vec<BrandOpenPitch>, GeneralNagError> {
        slack_firm_open_pitch_counts(&self.0, self.1)
            .await
            .map_err(|_| GeneralNagError::Counts)
    }
}

struct ApplicationCounts {
    surreal: store::surreal::SurrealDb,
    postgres: store::postgres::PostgresDb,
}

#[async_trait::async_trait]
impl CountSource for ApplicationCounts {
    async fn read(&self) -> Result<Vec<BrandOpenPitch>, GeneralNagError> {
        store::postgres::PostgresDb::with_application_db(
            self.postgres.clone(),
            slack_firm_open_pitch_counts(&self.surreal, DeploymentSlackFirm::DefaultBrand),
        )
        .await
        .map_err(|_| GeneralNagError::Counts)?
        .map_err(|_| GeneralNagError::Counts)
    }
}

impl Context {
    /// Inject delivery and an owned store whose admission is checked on each read.
    pub fn with_store(
        surreal: store::surreal::SurrealDb,
        postgres: store::postgres::PostgresDb,
        slack: Arc<dyn SlackBot>,
        channel_id: String,
        simulated: bool,
    ) -> Self {
        Self {
            slack,
            channel_id,
            simulated,
            counts: Arc::new(ApplicationCounts { surreal, postgres }),
        }
    }

    /// Post with the deployment's bot to `SLACK_GENERAL_CHANNEL_ID`, which an
    /// enabled schedule requires, counting the matters of every Brand the
    /// deployment's Slack Firm owns.
    pub fn from_lookup<F>(
        surreal: store::surreal::SurrealDb,
        slack: Arc<dyn SlackBot>,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let firm = DeploymentSlackFirm::from_lookup(&lookup)
            .map_err(|_| super::SettingsError::Invalid(SLACK_FIRM_ENV))?;
        let channel_id = super::required_channel(lookup, "SLACK_GENERAL_CHANNEL_ID")?;
        Ok(Self {
            slack,
            channel_id,
            simulated,
            counts: Arc::new(SurrealCounts(surreal, firm)),
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

async fn post_once(
    slack: &dyn SlackBot,
    channel: &str,
    text: &str,
    oldest: DateTime<Utc>,
) -> dbos::Result<(), GeneralNagError> {
    if super::post_once(slack, channel, text, oldest).await {
        Ok(())
    } else {
        Err(dbos::Error::Application(GeneralNagError::Notify))
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<GeneralNagWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            if context.channel_id.is_empty() {
                return Err(dbos::Error::Application(GeneralNagError::Notify));
            }
            let oldest = dbos::workflow_id()
                .and_then(|id| {
                    id.strip_prefix(&format!("{WORKFLOW}-"))
                        .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
                })
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|time| time.and_utc())
                .ok_or(dbos::Error::Application(GeneralNagError::Notify))?;
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
                        post_once(
                            slack.as_ref(),
                            &channel_id,
                            "Nobody Cares, Work Harder",
                            oldest,
                        )
                        .await
                    }
                },
            )
            .await?;

            let mut posted_followup = false;
            if !context.simulated {
                let counts = Arc::clone(&context.counts);
                let brands = dbos::step("counts", move || {
                    let counts = Arc::clone(&counts);
                    async move { counts.read().await.map_err(dbos::Error::Application) }
                })
                .await?;
                let followup = open_matters_message(&brands);
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
                            post_once(slack.as_ref(), &channel_id, &followup, oldest).await
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
        async fn read(
            &self,
        ) -> Result<Vec<store::projects::BrandOpenPitch>, super::GeneralNagError> {
            Ok(Vec::new())
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
    fn enabled_schedule_requires_its_general_channel() {
        let context = |channel: Option<&'static str>| {
            super::Context::from_lookup(
                store::surreal::test_support::unreachable(),
                Arc::new(crate::CapturingSlackBot::new()),
                true,
                move |key| (key == "SLACK_GENERAL_CHANNEL_ID").then_some(channel?.to_owned()),
            )
        };
        for blank in [None, Some("  ")] {
            assert!(matches!(
                context(blank),
                Err(super::super::SettingsError::Missing(
                    "SLACK_GENERAL_CHANNEL_ID"
                ))
            ));
        }
        assert!(context(Some("C_GENERAL")).is_ok());
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
