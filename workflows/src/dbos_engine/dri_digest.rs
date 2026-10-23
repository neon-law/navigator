//! DBOS port of the nightly project DRI operations digest.
//!
//! The workflow keeps the existing digest query and message semantics. Its
//! stable daily id lets replicas and restarts join the same scheduled run.
//! Slack's incoming webhook has no idempotency key, so a crash after delivery
//! and before the checkpoint can post a duplicate.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::projects::ProjectDriSummary;

pub use crate::dri_digest::dri_digest_message;
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
    async fn open_pitch_counts(&self) -> Result<(usize, usize), DriDigestError>;
}

struct SurrealDriDigestSource(store::surreal::SurrealDb);

#[async_trait::async_trait]
impl DriDigestSource for SurrealDriDigestSource {
    async fn projects(&self) -> Result<Vec<ProjectDriSummary>, DriDigestError> {
        store::projects::dri_digest(&self.0)
            .await
            .map_err(|_| DriDigestError::Projects)
    }

    async fn open_pitch_counts(&self) -> Result<(usize, usize), DriDigestError> {
        store::projects::matter_open_pitch_counts(&self.0)
            .await
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
    /// Construct a host context from the same SurrealDB queries used by the
    /// Restate service. The webhook is required whenever this schedule is
    /// enabled; the generic notifier's capture-only fallback is not delivery.
    pub fn from_lookup<F>(
        surreal: store::surreal::SurrealDb,
        notifier: Arc<dyn Notifier>,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        validate_delivery_config(&lookup)?;
        Ok(Self {
            source: Arc::new(SurrealDriDigestSource(surreal)),
            notifier,
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

fn validate_delivery_config(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<(), super::SettingsError> {
    lookup("SLACK_WEBHOOK_URL")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(super::SettingsError::Missing("SLACK_WEBHOOK_URL"))?;
    Ok(())
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
                let (open, pitches) = dbos::step("counts", move || {
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
                let followup = format!("Total open matters: {open}, pitches: {pitches}");
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
mod tests {
    use super::{
        enabled, slot, validate_delivery_config, workflow_id, DriDigestSource, ENABLED_ENV,
    };
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
    fn enabled_schedule_requires_a_nonblank_slack_webhook() {
        assert_eq!(
            validate_delivery_config(|_| None).unwrap_err(),
            super::super::SettingsError::Missing("SLACK_WEBHOOK_URL")
        );
        assert_eq!(
            validate_delivery_config(|_| Some("  ".into())).unwrap_err(),
            super::super::SettingsError::Missing("SLACK_WEBHOOK_URL")
        );
        assert!(validate_delivery_config(|_| Some("https://example.test/hook".into())).is_ok());
    }

    #[tokio::test]
    async fn surreal_source_keeps_the_existing_all_project_both_sides_query() {
        use store::persons::{self, NewPerson};
        use store::projects::{create, designate_dri_in_surreal, DriSide, NewProject};

        let surreal = store::test_support::mem_surreal().await;
        let lawyer = persons::create(
            &surreal,
            &NewPerson::new("Synthetic Lawyer", "dri-lawyer@example.test"),
        )
        .await
        .expect("create synthetic lawyer");
        let client = persons::create(
            &surreal,
            &NewPerson::new("Synthetic Client", "dri-client@example.test"),
        )
        .await
        .expect("create synthetic client");
        let assigned = create(
            &surreal,
            &NewProject {
                code: "dri-assigned".into(),
                name: "Private display name".into(),
                status: "open".into(),
                entity_id: uuid::Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .expect("create assigned project");
        create(
            &surreal,
            &NewProject {
                code: "dri-unassigned".into(),
                name: "Another private name".into(),
                status: "open".into(),
                entity_id: uuid::Uuid::now_v7(),
                ..Default::default()
            },
        )
        .await
        .expect("create unassigned project");
        designate_dri_in_surreal(&surreal, assigned.id, lawyer.id, DriSide::Lawyer)
            .await
            .expect("designate lawyer DRI");
        designate_dri_in_surreal(&surreal, assigned.id, client.id, DriSide::Client)
            .await
            .expect("designate client DRI");

        let rows = super::SurrealDriDigestSource(surreal)
            .projects()
            .await
            .expect("read project DRI summary");
        assert_eq!(rows.len(), 2);
        let assigned = rows
            .iter()
            .find(|project| project.code == "dri-assigned")
            .expect("assigned project is present");
        assert_eq!(assigned.lawyer_dris, ["Synthetic Lawyer"]);
        assert_eq!(assigned.client_dris, ["Synthetic Client"]);
        let unassigned = rows
            .iter()
            .find(|project| project.code == "dri-unassigned")
            .expect("unassigned project is present");
        assert!(unassigned.lawyer_dris.is_empty());
        assert!(unassigned.client_dris.is_empty());
    }
}
