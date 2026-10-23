//! DBOS schedule that runs the governed retention purge once a day.
//!
//! Each soft-deleted chain past its 180-day window is purged in its own step,
//! authorized by the person a deployment configured for the chain's own Firm
//! (`NAVIGATOR_RETENTION_PURGE_AUTHORITIES`). The purge rechecks Legal Holds,
//! eligibility, and that authority from fresh rows inside the step, so a hold
//! placed while a run is pending still protects the chain, and a retry joins
//! the recorded run instead of expunging twice. The daily workflow id makes
//! replicas and restarts join one run. Journals carry Project ids and chain
//! keys, never document content.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKFLOW: &str = "RetentionPurge";
/// Independent opt-in, beside the authority map it requires.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_RETENTION_PURGE_ENABLED";
/// `<firm-uuid>=<person-uuid>` pairs naming each Firm's purge authority.
pub const AUTHORITIES_ENV: &str = "NAVIGATOR_RETENTION_PURGE_AUTHORITIES";
const RETRY_START: Duration = Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type RetentionPurgeWorkflow = WorkflowRef<(), PurgeRunReport, PurgeQueueError>;

/// What one day's run did, per chain.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeRunReport {
    /// `(project, chain key, outcome)` for every chain past its window.
    pub chains: Vec<(Uuid, String, String)>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum PurgeQueueError {
    #[error("chains past their retention window could not be read")]
    Queue,
}

/// The purge a run performs, behind a trait so the host supplies the governed
/// expunge and tests can drive the schedule.
#[async_trait::async_trait]
pub trait RetentionSource: Send + Sync {
    /// Chains past their window as of `now`.
    async fn due(&self, now: DateTime<Utc>) -> Result<Vec<(Uuid, String)>, PurgeQueueError>;
    /// Purge one chain; the outcome summary names what happened.
    async fn purge(&self, project_id: Uuid, chain_key: &str, now: DateTime<Utc>) -> String;
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn RetentionSource>,
}

impl Context {
    #[must_use]
    pub fn new(source: Arc<dyn RetentionSource>) -> Self {
        Self { source }
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

/// The configured authority map, required once the schedule is enabled.
pub fn authorities(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, super::SettingsError> {
    lookup(AUTHORITIES_ENV)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or(super::SettingsError::Missing(AUTHORITIES_ENV))
}

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// The day's slot: 03:17 UTC.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(3, 17, 0)
        .and_then(|time| time.and_local_timezone(Utc).single())
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%d"))
}

/// The slot instant a workflow id names, so every step of one run judges the
/// window at the same moment however late a recovery replays it.
fn slot_of(workflow_id: &str) -> Option<DateTime<Utc>> {
    let date = workflow_id.strip_prefix(&format!("{WORKFLOW}-"))?;
    let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    slot(date.and_hms_opt(0, 0, 0)?.and_utc()).into()
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<RetentionPurgeWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let now = dbos::workflow_id()
                .as_deref()
                .and_then(slot_of)
                .unwrap_or_else(Utc::now);
            let source = Arc::clone(&context.source);
            let due = dbos::step("due", move || {
                let source = Arc::clone(&source);
                async move { source.due(now).await.map_err(dbos::Error::Application) }
            })
            .await?;
            let mut report = PurgeRunReport::default();
            for (project_id, chain_key) in due {
                let source = Arc::clone(&context.source);
                let key = chain_key.clone();
                let outcome: String =
                    dbos::step(&format!("purge-{project_id}-{chain_key}"), move || {
                        let (source, key) = (Arc::clone(&source), key.clone());
                        async move {
                            dbos::Result::<_, PurgeQueueError>::Ok(
                                source.purge(project_id, &key, now).await,
                            )
                        }
                    })
                    .await?;
                report.chains.push((project_id, chain_key, outcome));
            }
            dbos::Result::<_, PurgeQueueError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &RetentionPurgeWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<PurgeRunReport, PurgeQueueError>, PurgeQueueError> {
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

pub async fn schedule(workflow: RetentionPurgeWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = if now >= current {
            match fire(&workflow, current).await {
                Ok(_) => (current + TimeDelta::hours(24) - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(current), %error, "retention purge start failed");
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
        authorities, enabled, slot, slot_of, system_clock, workflow_id, AUTHORITIES_ENV,
        ENABLED_ENV,
    };
    use crate::dbos_engine::SettingsError;
    use chrono::{TimeDelta, TimeZone, Utc};

    #[test]
    fn a_day_has_one_slot_and_its_id_names_the_instant_every_step_judges() {
        let early = Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 18, 3, 17, 0).unwrap();
        assert_eq!(slot(early), expected);
        assert_eq!(workflow_id(expected), "RetentionPurge-2026-10-18");
        assert_eq!(slot_of("RetentionPurge-2026-10-18"), Some(expected));
    }

    #[test]
    fn an_enabled_schedule_requires_its_authority_map() {
        assert!(!enabled(|_| None).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
        assert_eq!(
            authorities(|_| Some(" ".into())).unwrap_err(),
            super::super::SettingsError::Missing(AUTHORITIES_ENV)
        );
    }

    #[test]
    fn only_a_literal_true_enables_the_schedule() {
        let flag =
            |value: &'static str| move |key: &str| (key == ENABLED_ENV).then(|| value.into());
        assert!(enabled(flag("true")).unwrap());
        assert!(enabled(flag(" true ")).unwrap());
        assert!(!enabled(flag("false")).unwrap());
        assert!(!enabled(flag("")).unwrap());
        assert!(!enabled(flag("   ")).unwrap());
        for malformed in ["TRUE", "1", "on", "true,false"] {
            assert_eq!(
                enabled(flag(malformed)).unwrap_err(),
                SettingsError::Invalid(ENABLED_ENV),
                "{malformed}"
            );
        }
    }

    #[test]
    fn the_authority_map_is_read_trimmed_and_whole() {
        assert_eq!(
            authorities(|_| None).unwrap_err(),
            SettingsError::Missing(AUTHORITIES_ENV)
        );
        assert_eq!(
            authorities(|_| Some(String::new())).unwrap_err(),
            SettingsError::Missing(AUTHORITIES_ENV)
        );
        let firms = "00000000-0000-7000-8000-000000000001=00000000-0000-7000-8000-000000000002,\
                     00000000-0000-7000-8000-000000000003=00000000-0000-7000-8000-000000000004";
        let read = authorities(|key| (key == AUTHORITIES_ENV).then(|| format!("  {firms}\n")));
        assert_eq!(read.unwrap(), firms, "every Firm's pair reaches the parser");
        // Malformed pairs are the parser's to refuse; this reader only
        // requires that something is configured.
        assert_eq!(
            authorities(|_| Some("not-a-pair".into())).unwrap(),
            "not-a-pair"
        );
    }

    #[test]
    fn a_foreign_or_malformed_workflow_id_names_no_slot() {
        assert_eq!(slot_of("ProjectRecovery-2026-10-18"), None);
        assert_eq!(slot_of("RetentionPurge-yesterday"), None);
        assert_eq!(slot_of("RetentionPurge-2026-13-40"), None);
        let late = Utc.with_ymd_and_hms(2026, 10, 18, 23, 59, 59).unwrap();
        assert_eq!(
            slot(late),
            Utc.with_ymd_and_hms(2026, 10, 18, 3, 17, 0).unwrap(),
            "a late instant still names that day's slot"
        );
    }

    #[test]
    fn the_system_clock_reads_now() {
        let before = Utc::now();
        let read = system_clock()();
        assert!(read >= before && read - before < TimeDelta::minutes(1));
    }
}
