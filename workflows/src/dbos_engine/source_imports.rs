//! DBOS schedule that applies pending Project source imports.
//!
//! The import door validates an archive and records a durable request; this
//! schedule applies each pending one every five minutes. The queue is the
//! `project_source_import` table, so a pod loss, restart, or unknown forge
//! outcome leaves a request the next slot retries. Each import runs in its own
//! step. A re-run is safe: the stored canonical archive is re-validated, the
//! first commit is found by its tree digest, and the Project row changes only
//! after the forge reports that commit ([`store::project_source_imports`]).
//! Journals carry import ids and reasons, never archive contents.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use dbos::{CancellationToken, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKFLOW: &str = "SourceImport";
/// Independent opt-in: hosting DBOS alone never writes to a forge.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED";
/// How many pending imports one slot applies.
pub const BATCH: i64 = 10;
const SLOT_MINUTES: i64 = 5;
const RETRY_START: Duration = Duration::from_mins(1);

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type SourceImportWorkflow = WorkflowRef<(), ImportReport, ImportQueueError>;

/// What one slot did. Identifiers and reasons only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub committed: Vec<Uuid>,
    pub stopped: Vec<(Uuid, String)>,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ImportQueueError {
    #[error("pending source imports could not be read")]
    Queue,
}

/// The work a slot performs, behind a trait so tests can drive it.
#[async_trait::async_trait]
pub trait ImportSource: Send + Sync {
    async fn pending(&self) -> Result<Vec<Uuid>, ImportQueueError>;
    async fn apply(&self, import_id: Uuid) -> Result<(), String>;
}

/// The production source: each import's forge is resolved for its Project's
/// own Firm, with that Firm's encrypted token.
pub struct StoreImportSource {
    surreal: store::surreal::SurrealDb,
    pg: store::postgres::PostgresDb,
    kms: Arc<dyn cloud::RuntimeKms>,
}

impl StoreImportSource {
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        pg: store::postgres::PostgresDb,
        kms: Arc<dyn cloud::RuntimeKms>,
    ) -> Self {
        Self { surreal, pg, kms }
    }
}

#[async_trait::async_trait]
impl ImportSource for StoreImportSource {
    async fn pending(&self) -> Result<Vec<Uuid>, ImportQueueError> {
        Ok(store::project_source_imports::pending(&self.pg, BATCH)
            .await
            .map_err(|_| ImportQueueError::Queue)?
            .into_iter()
            .map(|import| import.id)
            .collect())
    }

    async fn apply(&self, import_id: Uuid) -> Result<(), String> {
        let project_id = store::project_source_imports::get(&self.pg, import_id)
            .await
            .map_err(|error| error.to_string())?
            .project_id;
        let forge = store::project_surfaces::forgejo_for_project(
            &self.surreal,
            project_id,
            self.kms.as_ref(),
            |key| std::env::var(key).ok(),
        )
        .await
        .map_err(|error| error.to_string())?;
        store::project_source_imports::process(
            &self.surreal,
            &self.pg,
            &forge,
            import_id,
            Utc::now(),
        )
        .await
        .map(drop)
        .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub struct Context {
    source: Arc<dyn ImportSource>,
}

impl Context {
    #[must_use]
    pub fn new(source: Arc<dyn ImportSource>) -> Self {
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

/// The five-minute slot `now` falls in.
#[must_use]
pub fn slot(now: DateTime<Utc>) -> DateTime<Utc> {
    now.duration_trunc(TimeDelta::minutes(SLOT_MINUTES))
        .unwrap_or(now)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    format!("{WORKFLOW}-{}", slot.format("%Y-%m-%dT%H:%M"))
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<SourceImportWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let source = Arc::clone(&context.source);
            let queue = dbos::step("pending", move || {
                let source = Arc::clone(&source);
                async move { source.pending().await.map_err(dbos::Error::Application) }
            })
            .await?;
            let mut report = ImportReport::default();
            for import_id in queue {
                let source = Arc::clone(&context.source);
                let outcome: Result<(), String> =
                    dbos::step(&format!("import-{import_id}"), move || {
                        let source = Arc::clone(&source);
                        async move {
                            dbos::Result::<_, ImportQueueError>::Ok(source.apply(import_id).await)
                        }
                    })
                    .await?;
                match outcome {
                    Ok(()) => report.committed.push(import_id),
                    Err(reason) => {
                        tracing::warn!(%import_id, %reason, "source import stopped");
                        report.stopped.push((import_id, reason));
                    }
                }
            }
            dbos::Result::<_, ImportQueueError>::Ok(report)
        }
    })
}

pub async fn fire(
    workflow: &SourceImportWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<ImportReport, ImportQueueError>, ImportQueueError> {
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

pub async fn schedule(workflow: SourceImportWorkflow, clock: Clock, stop: CancellationToken) {
    loop {
        let now = clock();
        let current = slot(now);
        let wait = match fire(&workflow, current).await {
            Ok(_) => (current + TimeDelta::minutes(SLOT_MINUTES) - now)
                .to_std()
                .unwrap_or(Duration::ZERO),
            Err(error) => {
                tracing::error!(workflow_id = workflow_id(current), %error, "source import start failed");
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
    use super::{enabled, slot, system_clock, workflow_id, ImportQueueError, ENABLED_ENV};
    use chrono::{TimeDelta, TimeZone, Utc};

    #[test]
    fn every_instant_in_five_minutes_shares_one_slot() {
        let start = Utc.with_ymd_and_hms(2026, 10, 18, 9, 35, 0).unwrap();
        let late = Utc.with_ymd_and_hms(2026, 10, 18, 9, 39, 59).unwrap();
        assert_eq!(slot(late), start);
        assert_eq!(workflow_id(start), "SourceImport-2026-10-18T09:35");
    }

    #[test]
    fn schedule_requires_a_literal_true_flag() {
        assert!(!enabled(|_| None).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("on".into())).unwrap_err(),
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
            enabled(|_| Some("yes".into())).unwrap_err(),
            super::super::SettingsError::Invalid(ENABLED_ENV)
        );
    }

    #[test]
    fn the_system_clock_reads_now_and_errors_name_no_content() {
        let before = Utc::now();
        let read = system_clock()();
        assert!(read >= before && read - before < TimeDelta::minutes(1));
        assert_eq!(
            ImportQueueError::Queue.to_string(),
            "pending source imports could not be read"
        );
    }
}
