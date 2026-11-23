//! Repairs Project Calendar membership from current assignments. A time slot
//! deduplicates submissions, while each attempt reads current authority under
//! the store's calendar write lock.

use std::{sync::Arc, time::Duration};

use dbos::{CancellationToken, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::project_calendar::ParticipantSync;
use uuid::Uuid;

const WORKFLOW: &str = "ProjectCalendarMembership";
const BACKUP: &str = "ProjectCalendarEventBackup";
const PERIOD: i64 = 300;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{phase}: {status:?} (participant {person_id:?}, attempts {attempts})")]
pub struct SyncError {
    pub phase: String,
    pub status: Option<u16>,
    pub person_id: Option<Uuid>,
    pub attempts: u8,
}

#[async_trait::async_trait]
pub trait Source: Send + Sync {
    async fn candidates(&self) -> Result<Vec<Uuid>, SyncError>;
    async fn backup(&self, project_id: Uuid) -> Result<u64, SyncError>;
    async fn sync(&self, project_id: Uuid) -> Result<Vec<ParticipantSync>, SyncError>;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncReport {
    pub participants: Vec<ParticipantSync>,
    pub attempts: u8,
}

pub type Workflow = WorkflowRef<Uuid, SyncReport, SyncError>;

pub fn register(dbos: &DBOS, source: Arc<dyn Source>) -> dbos::Result<Workflow> {
    dbos.register_workflow(WORKFLOW, move |project_id| {
        let source = source.clone();
        async move {
            let mut attempt = 1_u8;
            loop {
                let source = source.clone();
                let result = dbos::step(&format!("sync-current-membership-{attempt}"), move || {
                    let source = source.clone();
                    async move { dbos::Result::<_, SyncError>::Ok(source.sync(project_id).await) }
                })
                .await?;
                match result {
                    Ok(participants) => {
                        return Ok(SyncReport {
                            participants,
                            attempts: attempt,
                        })
                    }
                    Err(mut error) => {
                        error.attempts = attempt;
                        let transient = error.status.is_none_or(|status| {
                            matches!(status, 408 | 409 | 412 | 429 | 500..=599)
                        });
                        if !transient || attempt == 3 {
                            return Err(dbos::Error::Application(error));
                        }
                        dbos::sleep::<SyncError>(Duration::from_secs(15 * u64::from(attempt)))
                            .await?;
                        attempt += 1;
                    }
                }
            }
        }
    })
}

pub(super) fn register_schedule(
    dbos: &DBOS,
    source: Arc<dyn Source>,
) -> dbos::Result<super::ScheduleStarter> {
    let workflow = register(dbos, source.clone())?;
    let archives = source.clone();
    let backup = dbos.register_workflow(BACKUP, move |project_id: Uuid| {
        let source = archives.clone();
        async move {
            dbos::step_with(
                "save-event-revisions",
                dbos::StepOptions {
                    max_attempts: 3,
                    ..Default::default()
                },
                move || {
                    let source = source.clone();
                    async move {
                        source
                            .backup(project_id)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                },
            )
            .await
        }
    })?;
    Ok(Box::new(move |stop| {
        tokio::spawn(schedule(workflow, backup, source, stop))
    }))
}

async fn schedule(
    workflow: Workflow,
    backup: WorkflowRef<Uuid, u64, SyncError>,
    source: Arc<dyn Source>,
    stop: CancellationToken,
) {
    loop {
        let now = chrono::Utc::now().timestamp();
        let slot = now.div_euclid(PERIOD);
        let hour = now.div_euclid(3600);
        match source.candidates().await {
            Ok(projects) => {
                for project_id in projects {
                    if stop.is_cancelled() {
                        return;
                    }
                    let id = format!("{WORKFLOW}:{project_id}:{slot}");
                    if let Err(error) = workflow
                        .start_with(
                            project_id,
                            dbos::StartOptions {
                                workflow_id: Some(&id),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        tracing::warn!(workflow_id = id, %error, "Calendar membership submission failed");
                    }
                    let id = format!("{BACKUP}:{project_id}:{hour}");
                    if let Err(error) = backup
                        .start_with(
                            project_id,
                            dbos::StartOptions {
                                workflow_id: Some(&id),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        tracing::warn!(workflow_id = id, %error, "Calendar event backup submission failed");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "Calendar membership candidates unavailable"),
        }
        tokio::select! {
            () = stop.cancelled() => return,
            // Retry a missed submission while reusing the current slot's id.
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}
