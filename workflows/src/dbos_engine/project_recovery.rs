//! DBOS schedule that drives Project Recovery Archive generations to commit.
//!
//! A request door records a generation in PostgreSQL and returns; this
//! schedule resumes every unfinished generation each quarter hour. The work
//! queue is the generation table itself, so a pod loss, a restart, or a
//! provider timeout leaves a durable request the next slot picks up. Each
//! generation advances in its own step, and every step is an idempotent
//! copy-and-verify keyed by content and path
//! ([`store::project_archives`]): a crash between a step and its checkpoint
//! repeats only verified-idempotent work. The slot-keyed workflow id makes
//! replicas join one run per slot.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKFLOW: &str = "ProjectRecovery";
/// Independent opt-in: hosting DBOS alone never starts archive work.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED";
/// How many unfinished generations one slot advances.
pub const BATCH: i64 = 25;
/// How many soft-deleted rows one slot considers for archiving.
pub const SOFT_DELETED_BATCH: usize = 500;
const SLOT_MINUTES: i64 = 15;
const RETRY_START: Duration = Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type ProjectRecoveryWorkflow = WorkflowRef<(), RecoveryReport, RecoveryError>;

/// What one slot did. Identifiers and counts only, never content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub committed: Vec<Uuid>,
    /// Generations that stopped, each with its recorded reason.
    pub stalled: Vec<(Uuid, String)>,
    /// What archiving soft-deleted chains did this slot, or why it stopped.
    pub soft_deleted: Option<String>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum RecoveryError {
    #[error("unfinished recovery archives could not be read")]
    Queue,
}

/// The work a slot performs, behind a trait so tests can drive it.
#[async_trait::async_trait]
pub trait RecoverySource: Send + Sync {
    /// Unfinished generations, oldest first.
    async fn unfinished(&self) -> Result<Vec<Uuid>, RecoveryError>;
    /// Advance one generation; `Err` carries the recorded reason it stopped.
    async fn advance(&self, generation_id: Uuid) -> Result<(), String>;
    /// Archive soft-deleted chains and remove verified active copies; returns
    /// a summary of counts.
    async fn archive_soft_deleted(&self) -> Result<String, String> {
        Ok("not configured".into())
    }
}

/// The production source: SurrealDB, PostgreSQL, the documents lane, the
/// private archived-projects lane, and the optional Git bundle lane.
pub struct StoreRecoverySource {
    surreal: store::surreal::SurrealDb,
    pg: store::postgres::PostgresDb,
    documents: Arc<dyn cloud::StorageService>,
    archive: Arc<dyn cloud::StorageService>,
    source_backups: Option<Arc<dyn cloud::StorageService>>,
}

impl StoreRecoverySource {
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        pg: store::postgres::PostgresDb,
        documents: Arc<dyn cloud::StorageService>,
        archive: Arc<dyn cloud::StorageService>,
        source_backups: Option<Arc<dyn cloud::StorageService>>,
    ) -> Self {
        Self {
            surreal,
            pg,
            documents,
            archive,
            source_backups,
        }
    }
}

#[async_trait::async_trait]
impl RecoverySource for StoreRecoverySource {
    async fn unfinished(&self) -> Result<Vec<Uuid>, RecoveryError> {
        Ok(store::project_archives::unfinished(&self.pg, BATCH)
            .await
            .map_err(|_| RecoveryError::Queue)?
            .into_iter()
            .map(|generation| generation.id)
            .collect())
    }

    async fn advance(&self, generation_id: Uuid) -> Result<(), String> {
        let stores = store::project_archives::ArchiveStores {
            surreal: &self.surreal,
            pg: &self.pg,
            documents: &self.documents,
            archive: &self.archive,
            source_backups: self.source_backups.as_ref(),
        };
        store::project_archives::resume_unattended(&stores, generation_id, Utc::now())
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    async fn archive_soft_deleted(&self) -> Result<String, String> {
        let stores = store::document_retention::RetentionStores {
            surreal: &self.surreal,
            pg: &self.pg,
            documents: &self.documents,
            archive: &self.archive,
        };
        let report = store::document_retention::reconcile_soft_deleted(
            &stores,
            Utc::now(),
            SOFT_DELETED_BATCH,
        )
        .await
        .map_err(|error| error.to_string())?;
        Ok(format!(
            "archived {}, active copies removed {}, without Firm {}, stalled {}",
            report.archived,
            report.active_removed,
            report.without_firm,
            report.stalled.len()
        ))
    }
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn RecoverySource>,
}

impl Context {
    #[must_use]
    pub fn new(source: Arc<dyn RecoverySource>) -> Self {
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

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// The quarter hour `now` falls in.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.duration_trunc(TimeDelta::minutes(SLOT_MINUTES))
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%dT%H:%M"))
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<ProjectRecoveryWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let source = Arc::clone(&context.source);
            let queue = dbos::step("unfinished", move || {
                let source = Arc::clone(&source);
                async move { source.unfinished().await.map_err(dbos::Error::Application) }
            })
            .await?;
            let mut report = RecoveryReport::default();
            for generation_id in queue {
                let source = Arc::clone(&context.source);
                let outcome: Result<(), String> =
                    dbos::step(&format!("archive-{generation_id}"), move || {
                        let source = Arc::clone(&source);
                        async move {
                            dbos::Result::<_, RecoveryError>::Ok(
                                source.advance(generation_id).await,
                            )
                        }
                    })
                    .await?;
                match outcome {
                    Ok(()) => report.committed.push(generation_id),
                    Err(reason) => {
                        tracing::warn!(%generation_id, %reason, "recovery archive stalled");
                        report.stalled.push((generation_id, reason));
                    }
                }
            }
            let source = Arc::clone(&context.source);
            let soft_deleted: Result<String, String> =
                dbos::step("soft-deleted", move || {
                    let source = Arc::clone(&source);
                    async move {
                        dbos::Result::<_, RecoveryError>::Ok(source.archive_soft_deleted().await)
                    }
                })
                .await?;
            report.soft_deleted = Some(soft_deleted.unwrap_or_else(|reason| reason));
            dbos::Result::<_, RecoveryError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &ProjectRecoveryWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<RecoveryReport, RecoveryError>, RecoveryError> {
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

pub async fn schedule(workflow: ProjectRecoveryWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = match fire(&workflow, current).await {
            Ok(_) => (current + TimeDelta::minutes(SLOT_MINUTES) - now)
                .to_std()
                .unwrap_or(Duration::ZERO),
            Err(error) => {
                tracing::error!(workflow_id = workflow_id(current), %error, "project recovery start failed");
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
    use super::{enabled, slot, system_clock, workflow_id, RecoveryError, ENABLED_ENV};
    use chrono::{TimeDelta, TimeZone, Utc};

    #[test]
    fn every_instant_in_a_quarter_hour_shares_one_slot() {
        let early = Utc.with_ymd_and_hms(2026, 10, 18, 9, 30, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 10, 18, 9, 44, 59).unwrap();
        assert_eq!(slot(early), early);
        assert_eq!(slot(late), early);
        assert_eq!(workflow_id(slot(late)), "ProjectRecovery-2026-10-18T09:30");
    }

    #[test]
    fn schedule_requires_a_literal_true_flag() {
        assert!(!enabled(|_| None).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("1".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }

    #[test]
    fn blank_and_false_flags_leave_the_schedule_off() {
        for off in ["", "  ", "false", " false "] {
            assert!(!enabled(|_| Some(off.into())).unwrap(), "{off:?}");
        }
        assert!(enabled(|_| Some(" true ".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("TRUE".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }

    #[test]
    fn the_system_clock_reads_now_and_errors_name_no_content() {
        let before = Utc::now();
        let read = system_clock()();
        assert!(read >= before && read - before < TimeDelta::minutes(1));
        assert_eq!(
            RecoveryError::Queue.to_string(),
            "unfinished recovery archives could not be read"
        );
    }
}
