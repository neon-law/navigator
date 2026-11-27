//! DBOS port of the nightly project DRI operations digest.
//!
//! The workflow keeps the existing digest query and message semantics. Its
//! stable daily id lets replicas and restarts join the same scheduled run.
//! Its posts carry no receipt correlation, so a crash after delivery and
//! before the checkpoint can post a duplicate.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::projects::{BrandOpenPitch, ProjectDriSummary};

pub use crate::dri_digest::dri_digest_message;
use crate::dri_digest::{open_matters_message, slack_firm_open_pitch_counts};
use crate::firm_slack::{DeploymentSlackFirm, SLACK_FIRM_ENV};
use crate::Notifier;

pub const WORKFLOW: &str = "DriDigest";
/// Independent opt-in keeps the Restate CronJob as the active schedule until
/// the deployment explicitly switches engines.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_DRI_DIGEST_ENABLED";
const RETRY_START: Duration = Duration::from_mins(1);
const NOTIFY_ATTEMPTS: u32 = 5;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type DriDigestWorkflow = WorkflowRef<(), DriDigestReport, DriDigestError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriDigestReport {
    pub projects: usize,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum DriDigestError {
    #[error("the project DRI digest could not be read")]
    Projects,
    #[error("the open-matters count could not be read")]
    Counts,
    #[error("the DRI digest notice was not delivered")]
    Notify,
}

#[async_trait::async_trait]
pub trait DriDigestSource: Send + Sync {
    async fn projects(&self) -> Result<Vec<ProjectDriSummary>, DriDigestError>;
    async fn open_pitch_counts(&self) -> Result<Vec<BrandOpenPitch>, DriDigestError>;
}

struct SurrealDriDigestSource(store::surreal::SurrealDb, DeploymentSlackFirm);

#[async_trait::async_trait]
impl DriDigestSource for SurrealDriDigestSource {
    async fn projects(&self) -> Result<Vec<ProjectDriSummary>, DriDigestError> {
        store::projects::dri_digest(&self.0)
            .await
            .map_err(|_| DriDigestError::Projects)
    }

    async fn open_pitch_counts(&self) -> Result<Vec<BrandOpenPitch>, DriDigestError> {
        slack_firm_open_pitch_counts(&self.0, self.1)
            .await
            .map_err(|_| DriDigestError::Counts)
    }
}

struct ApplicationDriDigestSource {
    surreal: store::surreal::SurrealDb,
    postgres: store::postgres::PostgresDb,
    firm: DeploymentSlackFirm,
}

#[async_trait::async_trait]
impl DriDigestSource for ApplicationDriDigestSource {
    async fn projects(&self) -> Result<Vec<ProjectDriSummary>, DriDigestError> {
        store::postgres::PostgresDb::with_application_db(
            self.postgres.clone(),
            store::projects::dri_digest(&self.surreal),
        )
        .await
        .map_err(|_| DriDigestError::Projects)?
        .map_err(|_| DriDigestError::Projects)
    }
    async fn open_pitch_counts(&self) -> Result<Vec<BrandOpenPitch>, DriDigestError> {
        store::postgres::PostgresDb::with_application_db(
            self.postgres.clone(),
            slack_firm_open_pitch_counts(&self.surreal, self.firm),
        )
        .await
        .map_err(|_| DriDigestError::Counts)?
        .map_err(|_| DriDigestError::Counts)
    }
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn DriDigestSource>,
    notifier: Arc<dyn Notifier>,
    simulated: bool,
}

impl Context {
    /// Inject delivery and an owned store whose admission is checked on each read.
    pub fn with_store(
        surreal: store::surreal::SurrealDb,
        postgres: store::postgres::PostgresDb,
        notifier: Arc<dyn Notifier>,
        simulated: bool,
    ) -> Self {
        Self::with_source(
            Arc::new(ApplicationDriDigestSource {
                surreal,
                postgres,
                firm: DeploymentSlackFirm::DefaultBrand,
            }),
            notifier,
            simulated,
        )
    }

    /// Construct a host context from the same Project accessors used by the
    /// Restate service. The digest posts with the deployment's bot to
    /// `SLACK_GENERAL_CHANNEL_ID`, beside `GeneralNag`'s open-matters notice;
    /// an enabled schedule requires the channel.
    pub fn from_lookup<F>(
        surreal: store::surreal::SurrealDb,
        bot: Arc<dyn crate::SlackBot>,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let firm = DeploymentSlackFirm::from_lookup(&lookup)
            .map_err(|_| super::SettingsError::Invalid(SLACK_FIRM_ENV))?;
        let channel = super::required_channel(lookup, "SLACK_GENERAL_CHANNEL_ID")?;
        Ok(Self {
            source: Arc::new(SurrealDriDigestSource(surreal, firm)),
            notifier: Arc::new(crate::notify::ChannelNotifier::new(bot, channel)),
            simulated,
        })
    }

    /// Construct a context with a test or alternate data source.
    pub fn with_source(
        source: Arc<dyn DriDigestSource>,
        notifier: Arc<dyn Notifier>,
        simulated: bool,
    ) -> Self {
        Self {
            source,
            notifier,
            simulated,
        }
    }
}

/// Whether this schedule is enabled. Only literal `true` opts in.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, super::SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(super::SettingsError::Invalid(ENABLED_ENV)),
    }
}

#[must_use]
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

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<DriDigestWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let source = Arc::clone(&context.source);
            let projects = dbos::step("query", move || {
                let source = Arc::clone(&source);
                async move { source.projects().await.map_err(dbos::Error::Application) }
            })
            .await?;

            let rendered = dri_digest_message(&projects, context.simulated);
            let notifier = Arc::clone(&context.notifier);
            dbos::step_with(
                "notify",
                StepOptions {
                    max_attempts: NOTIFY_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let (notifier, rendered) = (Arc::clone(&notifier), rendered.clone());
                    async move {
                        notifier
                            .notify(rendered)
                            .await
                            .map_err(|_| dbos::Error::Application(DriDigestError::Notify))
                    }
                },
            )
            .await?;

            if !context.simulated {
                let source = Arc::clone(&context.source);
                let brands = dbos::step("counts", move || {
                    let source = Arc::clone(&source);
                    async move {
                        source
                            .open_pitch_counts()
                            .await
                            .map_err(dbos::Error::Application)
                    }
                })
                .await?;
                let notifier = Arc::clone(&context.notifier);
                let followup = open_matters_message(&brands);
                dbos::step_with(
                    "notify_counts",
                    StepOptions {
                        max_attempts: NOTIFY_ATTEMPTS,
                        ..StepOptions::default()
                    },
                    move || {
                        let (notifier, followup) = (Arc::clone(&notifier), followup.clone());
                        async move {
                            notifier
                                .notify(followup)
                                .await
                                .map_err(|_| dbos::Error::Application(DriDigestError::Notify))
                        }
                    },
                )
                .await?;
            }

            dbos::Result::<_, DriDigestError>::Ok(DriDigestReport {
                projects: projects.len(),
            })
        }
    })
}

pub async fn fire(
    workflow: &DriDigestWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<DriDigestReport, DriDigestError>, DriDigestError> {
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

pub async fn schedule(workflow: DriDigestWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = if now >= current {
            match fire(&workflow, current).await {
                Ok(_) => (current + Duration::from_hours(24) - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(current), %error, "DRI digest start failed");
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
#[path = "../../../workflows-service/tests/support/native_project.rs"]
mod native_project_fixture;

#[cfg(test)]
mod tests {
    use super::{enabled, slot, workflow_id, DriDigestSource, ENABLED_ENV};
    use chrono::{TimeZone, Utc};

    #[test]
    fn every_instant_on_a_utc_date_uses_the_same_daily_slot() {
        let early = Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 10, 13, 23, 59, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 13, 1, 11, 0).unwrap();
        assert_eq!(slot(early), expected);
        assert_eq!(slot(late), expected);
        assert_eq!(workflow_id(slot(early)), "DriDigest-2026-10-13");
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
    fn enabled_schedule_posts_to_the_general_channel() {
        let context = |channel: &'static str| {
            super::Context::from_lookup(
                store::surreal::SurrealDb::uninitialized(),
                std::sync::Arc::new(crate::CapturingSlackBot::new()),
                false,
                move |key| (key == "SLACK_GENERAL_CHANNEL_ID").then(|| channel.to_owned()),
            )
        };
        assert_eq!(
            context(" ").err().unwrap(),
            super::super::SettingsError::Missing("SLACK_GENERAL_CHANNEL_ID")
        );
        assert!(context("C0GENERAL").is_ok());
    }

    #[tokio::test]
    async fn native_source_lists_only_open_projects_with_both_dri_sides() {
        store::test_support::with_postgres(async {
            use store::persons::{self, NewPerson, Role};
            use store::projects::{designate_dri_in_surreal, DriSide};
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let surreal = store::surreal::SurrealDb::uninitialized();
            let (matter, admin) = super::native_project_fixture::matter(&surreal).await;
            let firm = matter
                .firm_id
                .expect("the native Project fixture has a Firm");
            let assigned = store::projects::find_by_id(&surreal, matter.id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(assigned.firm_id, Some(firm));
            assert_eq!(assigned.brand, matter.brand);
            assert_eq!(assigned.entity_id, matter.entity_id);
            assert!(persons::find_by_id(&surreal, admin)
                .await
                .unwrap()
                .is_some());
            let unassigned_project = super::native_project_fixture::new_project(
                &surreal,
                firm,
                &matter.brand,
                matter.entity_id,
            )
            .await;
            let unassigned = store::projects::find_by_id(&surreal, unassigned_project.id)
                .await
                .unwrap()
                .unwrap();
            let lawyer = persons::create(
                &surreal,
                &NewPerson::with_role("Synthetic Lawyer", "dri-lawyer@example.test", Role::Lawyer),
            )
            .await
            .unwrap();
            let client = persons::create(
                &surreal,
                &NewPerson::new("Synthetic Client", "dri-client@example.test"),
            )
            .await
            .unwrap();
            designate_dri_in_surreal(&surreal, matter.id, lawyer.id, DriSide::Lawyer)
                .await
                .unwrap();
            designate_dri_in_surreal(&surreal, matter.id, client.id, DriSide::Client)
                .await
                .unwrap();
            for status in ["closed", "archived"] {
                store::projects::create(
                    &surreal,
                    &store::projects::NewProject {
                        code: format!("synthetic-{status}"),
                        name: format!("Synthetic {status} project"),
                        status: status.into(),
                        entity_id: matter.entity_id,
                        firm_id: Some(firm),
                        brand: matter.brand.clone(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            }
            let slack_firm = crate::firm_slack::DeploymentSlackFirm::Configured(firm);
            let source = super::ApplicationDriDigestSource {
                surreal: surreal.clone(),
                postgres: pg.clone(),
                firm: slack_firm,
            };
            let rows = source.projects().await.unwrap();
            assert_eq!(rows.len(), 2);
            let assigned = rows.iter().find(|row| row.code == assigned.code).unwrap();
            assert_eq!(assigned.lawyer_dris, ["Synthetic Lawyer"]);
            assert_eq!(assigned.client_dris, ["Synthetic Client"]);
            let unassigned = rows.iter().find(|row| row.code == unassigned.code).unwrap();
            assert!(unassigned.lawyer_dris.is_empty());
            assert!(unassigned.client_dris.is_empty());
            // The Slack Firm's one Brand counts every matter not closed:
            // the two open Projects and the archived one.
            let open = |lines: Vec<store::projects::BrandOpenPitch>| {
                assert_eq!(lines.len(), 1, "{lines:?}");
                lines[0].open
            };
            assert_eq!(open(source.open_pitch_counts().await.unwrap()), 3);
            let strict_source = super::SurrealDriDigestSource(surreal, slack_firm);
            assert_eq!(strict_source.projects().await.unwrap().len(), 2);
            assert_eq!(open(strict_source.open_pitch_counts().await.unwrap()), 3);
            pg.pool().close().await;
            assert!(matches!(
                source.projects().await,
                Err(super::DriDigestError::Projects)
            ));
            assert!(matches!(
                source.open_pitch_counts().await,
                Err(super::DriDigestError::Counts)
            ));
        })
        .await;
    }
}
