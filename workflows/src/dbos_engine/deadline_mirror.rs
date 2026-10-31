//! DBOS schedule that reconciles every open matter's Deadline mirror
//! nightly.
//!
//! Each open Project's mirrored events are reconciled to its Deadline rows
//! in the Project's own step — one slow Google call never re-runs another
//! Project's pass, and a retry joins the recorded run instead of writing
//! twice. The daily workflow id makes replicas and restarts join one run.
//! A pass repairs what the row owns — summary, description, time, status,
//! sequence, reminder, identity properties — and deletes events whose row
//! is gone; a change made directly in Google is the person's and survives.
//! Journals carry Project ids and outcome counts, never client matter
//! content. The schedule applies (`dry_run: false`); a Context built with
//! [`Context::dry_run`] plans the same writes without making them, which is
//! how a run rehearses against the live calendar.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::deadline_calendar::MirrorReport;
use uuid::Uuid;

pub const WORKFLOW: &str = "DeadlineMirror";
/// Independent opt-in, beside the deployment's calendar credentials.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_DEADLINE_MIRROR_ENABLED";
const RETRY_START: Duration = Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type DeadlineMirrorWorkflow = WorkflowRef<(), MirrorRunReport, MirrorQueueError>;

/// What one night's run did, per matter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirrorRunReport {
    /// `(project, code, outcome line)` for every open matter.
    pub passes: Vec<(Uuid, String, String)>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum MirrorQueueError {
    #[error("open matters could not be read")]
    Queue,
}

/// The mirror a run performs, behind a trait so the host supplies the
/// credential-bearing calendar and tests can drive the schedule.
#[async_trait::async_trait]
pub trait MirrorSource: Send + Sync {
    /// The open matters, as `(id, code)` in a stable order.
    async fn open_projects(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, String)>, MirrorQueueError>;
    /// Reconcile one matter's mirror; a failure to reach Google or the
    /// store is an outcome line, not a failed run.
    async fn reconcile(
        &self,
        project_id: Uuid,
        dry_run: bool,
        now: DateTime<Utc>,
    ) -> Result<MirrorReport, String>;
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn MirrorSource>,
    dry_run: bool,
}

impl Context {
    #[must_use]
    pub fn new(source: Arc<dyn MirrorSource>) -> Self {
        Self {
            source,
            dry_run: false,
        }
    }

    /// A run that plans the night's writes without making them.
    #[must_use]
    pub fn dry_run(mut self) -> Self {
        self.dry_run = true;
        self
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

/// One matter's contribution to the run report: the pass line on success,
/// a `failed:` line when the reconcile itself broke. Extracted beside the
/// workflow step so the mapping is testable without a DBOS runtime.
async fn reconcile_line(
    source: &Arc<dyn MirrorSource>,
    project_id: Uuid,
    dry_run: bool,
    now: DateTime<Utc>,
) -> String {
    match source.reconcile(project_id, dry_run, now).await {
        Ok(pass) => store::deadline_calendar::report_line(&pass),
        Err(error) => format!("failed: {error}"),
    }
}

/// The day's slot: 04:03 UTC.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.date_naive()
        .and_hms_opt(4, 3, 0)
        .and_then(|time| time.and_local_timezone(Utc).single())
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%d"))
}

/// The slot instant a workflow id names, so every step of one run judges
/// the night at the same moment however late a recovery replays it.
fn slot_of(workflow_id: &str) -> Option<DateTime<Utc>> {
    let date = workflow_id.strip_prefix(&format!("{WORKFLOW}-"))?;
    let date = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    slot(date.and_hms_opt(0, 0, 0)?.and_utc()).into()
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<DeadlineMirrorWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let now = dbos::workflow_id()
                .as_deref()
                .and_then(slot_of)
                .unwrap_or_else(Utc::now);
            let dry_run = context.dry_run;
            let source = Arc::clone(&context.source);
            let open = dbos::step("open-projects", move || {
                let source = Arc::clone(&source);
                async move {
                    source
                        .open_projects(now)
                        .await
                        .map_err(dbos::Error::Application)
                }
            })
            .await?;
            let mut report = MirrorRunReport::default();
            for (project_id, code) in open {
                let source = Arc::clone(&context.source);
                let line: String = dbos::step(&format!("mirror-{project_id}"), move || {
                    let source = Arc::clone(&source);
                    async move {
                        dbos::Result::<_, MirrorQueueError>::Ok(
                            reconcile_line(&source, project_id, dry_run, now).await,
                        )
                    }
                })
                .await?;
                report.passes.push((project_id, code, line));
            }
            dbos::Result::<_, MirrorQueueError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &DeadlineMirrorWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<MirrorRunReport, MirrorQueueError>, MirrorQueueError> {
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

pub async fn schedule(workflow: DeadlineMirrorWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = if now >= current {
            match fire(&workflow, current).await {
                Ok(_) => (current + TimeDelta::hours(24) - now)
                    .to_std()
                    .unwrap_or(Duration::ZERO),
                Err(error) => {
                    tracing::error!(workflow_id = workflow_id(current), %error, "deadline mirror start failed");
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
    use std::sync::Arc;

    use super::{enabled, slot, slot_of, workflow_id, ENABLED_ENV};
    use crate::dbos_engine::SettingsError;
    use chrono::{DateTime, TimeZone, Utc};
    use store::deadline_calendar::MirrorReport;
    use uuid::Uuid;

    #[test]
    fn a_day_has_one_slot_and_its_id_names_the_instant_every_step_judges() {
        let early = Utc.with_ymd_and_hms(2026, 10, 30, 0, 0, 0).unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 30, 4, 3, 0).unwrap();
        assert_eq!(slot(early), expected);
        assert_eq!(workflow_id(expected), "DeadlineMirror-2026-10-30");
        assert_eq!(slot_of("DeadlineMirror-2026-10-30"), Some(expected));
        assert_eq!(slot_of("RetentionPurge-2026-10-30"), None);
    }

    #[test]
    fn only_a_literal_true_enables_the_schedule() {
        assert!(!enabled(|_| None).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            SettingsError::Invalid(ENABLED_ENV)
        );
        let flag =
            |value: &'static str| move |key: &str| (key == ENABLED_ENV).then(|| value.into());
        assert!(enabled(flag("true")).unwrap());
        assert!(enabled(flag(" true ")).unwrap());
        assert!(!enabled(flag("false")).unwrap());
        assert!(!enabled(flag("")).unwrap());
        for malformed in ["TRUE", "1", "on", "true,false"] {
            assert_eq!(
                enabled(flag(malformed)).unwrap_err(),
                SettingsError::Invalid(ENABLED_ENV),
                "{malformed}"
            );
        }
    }

    struct StubSource {
        report: Result<MirrorReport, String>,
    }

    #[async_trait::async_trait]
    impl super::MirrorSource for StubSource {
        async fn open_projects(
            &self,
            _now: DateTime<Utc>,
        ) -> Result<Vec<(Uuid, String)>, super::MirrorQueueError> {
            Ok(Vec::new())
        }

        async fn reconcile(
            &self,
            _project_id: Uuid,
            _dry_run: bool,
            _now: DateTime<Utc>,
        ) -> Result<MirrorReport, String> {
            self.report.clone()
        }
    }

    fn report() -> MirrorReport {
        MirrorReport {
            outcome: store::deadline_calendar::MirrorOutcome::Mirrored,
            dry_run: false,
            creates_calendar: false,
            upserted: vec![Uuid::now_v7()],
            deleted: Vec::new(),
            in_sync: 1,
            failures: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_passing_reconcile_becomes_the_report_line() {
        let source: Arc<dyn super::MirrorSource> = Arc::new(StubSource {
            report: Ok(report()),
        });
        let line = super::reconcile_line(&source, Uuid::now_v7(), false, Utc::now()).await;
        assert_eq!(line, "mirrored upserted=1 in_sync=1 deleted=0");
    }

    #[tokio::test]
    async fn a_broken_reconcile_is_an_outcome_line_not_a_failed_run() {
        let source: Arc<dyn super::MirrorSource> = Arc::new(StubSource {
            report: Err("google is unreachable".into()),
        });
        let line = super::reconcile_line(&source, Uuid::now_v7(), false, Utc::now()).await;
        assert_eq!(line, "failed: google is unreachable");
    }

    #[tokio::test]
    async fn a_planned_pass_says_so_in_its_line() {
        let mut planned = report();
        planned.dry_run = true;
        planned.upserted.clear();
        planned.in_sync = 0;
        let source: Arc<dyn super::MirrorSource> = Arc::new(StubSource {
            report: Ok(planned),
        });
        let line = super::reconcile_line(&source, Uuid::now_v7(), true, Utc::now()).await;
        assert_eq!(line, "mirrored upserted=0 in_sync=0 deleted=0 (dry run)");
    }
}
