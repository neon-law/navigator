//! Repairs Project Calendar membership from current assignments. A time slot
//! deduplicates submissions, while each attempt reads current authority under
//! the store's calendar write lock.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use dbos::{WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::project_calendar::ParticipantSync;
use uuid::Uuid;

const WORKFLOW: &str = "ProjectCalendarMembership";
const BACKUP: &str = "ProjectCalendarEventBackup";
const BOOKING: &str = "AttorneyBookingAvailability";

#[derive(Clone, Copy)]
enum Targets {
    Projects,
    Attorneys,
}

struct Cadence {
    period_seconds: i64,
    offset_seconds: i64,
}

const MEMBERSHIP_CADENCE: Cadence = Cadence {
    period_seconds: 6 * 3600,
    offset_seconds: 2 * 3600 + 22 * 60,
};
const BACKUP_CADENCE: Cadence = Cadence {
    period_seconds: 3600,
    offset_seconds: 0,
};

impl Cadence {
    /// The current UTC slot and the instant at which the next slot starts.
    fn slot(&self, now: DateTime<Utc>) -> (i64, DateTime<Utc>) {
        let index = (now.timestamp() - self.offset_seconds).div_euclid(self.period_seconds);
        let next = (index + 1) * self.period_seconds + self.offset_seconds;
        (index, DateTime::from_timestamp(next, 0).unwrap_or(now))
    }
}

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
    async fn booking_candidates(&self) -> Result<Vec<Uuid>, SyncError>;
    async fn backup(&self, project_id: Uuid) -> Result<u64, SyncError>;
    async fn sync(&self, project_id: Uuid) -> Result<Vec<ParticipantSync>, SyncError>;
    async fn sync_booking(&self, person_id: Uuid) -> Result<u64, SyncError>;
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
    let booking_source = source.clone();
    let booking = dbos.register_workflow(BOOKING, move |person_id: Uuid| {
        let source = booking_source.clone();
        async move {
            dbos::step_with(
                "reconcile-booking-availability",
                dbos::StepOptions {
                    max_attempts: 3,
                    ..Default::default()
                },
                move || {
                    let source = source.clone();
                    async move {
                        source
                            .sync_booking(person_id)
                            .await
                            .map_err(dbos::Error::Application)
                    }
                },
            )
            .await
        }
    })?;
    Ok(Box::new(move |stop| {
        tokio::spawn(async move {
            tokio::select! {
                biased;
                () = stop.cancelled() => {}
                () = schedule(&workflow, &source, MEMBERSHIP_CADENCE, Targets::Projects) => {}
                () = schedule(&backup, &source, BACKUP_CADENCE, Targets::Projects) => {}
                () = schedule(&booking, &source, BACKUP_CADENCE, Targets::Attorneys) => {}
            }
        })
    }))
}

async fn schedule<R>(
    workflow: &WorkflowRef<Uuid, R, SyncError>,
    source: &Arc<dyn Source>,
    cadence: Cadence,
    targets: Targets,
) where
    R: Serialize + serde::de::DeserializeOwned + Send + 'static,
{
    loop {
        let (slot, next) = cadence.slot(Utc::now());
        let mut submitted = true;
        let candidates = match targets {
            Targets::Projects => source.candidates().await,
            Targets::Attorneys => source.booking_candidates().await,
        };
        match candidates {
            Ok(candidates) => {
                for target in candidates {
                    let id = format!("{}:{target}:{slot}", workflow.name());
                    if let Err(error) = workflow
                        .start_with(
                            target,
                            dbos::StartOptions {
                                workflow_id: Some(&id),
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        tracing::warn!(workflow_id = id, %error, "Calendar workflow submission failed");
                        submitted = false;
                    }
                }
            }
            Err(error) => {
                tracing::warn!(workflow = workflow.name(), %error, "Calendar candidates unavailable");
                submitted = false;
            }
        }
        let until_next = (next - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        let wait = if submitted {
            until_next
        } else {
            until_next.min(Duration::from_mins(1))
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{BACKUP_CADENCE, MEMBERSHIP_CADENCE};
    use chrono::{DateTime, TimeDelta};

    #[test]
    fn membership_changes_slot_at_each_requested_utc_minute() {
        for time in [
            "2026-11-23T02:22:00Z",
            "2026-11-23T08:22:00Z",
            "2026-11-23T14:22:00Z",
            "2026-11-23T20:22:00Z",
            "2026-11-24T02:22:00Z",
        ] {
            let boundary = DateTime::parse_from_rfc3339(time).unwrap().to_utc();
            let earlier = boundary - TimeDelta::milliseconds(1);
            let (old_id, next) = MEMBERSHIP_CADENCE.slot(earlier);
            assert_eq!(next, boundary);
            let (new_id, next) = MEMBERSHIP_CADENCE.slot(boundary);
            assert_ne!(old_id, new_id);
            assert_eq!(next, boundary + TimeDelta::hours(6));
            assert_eq!(
                MEMBERSHIP_CADENCE.slot(next - TimeDelta::seconds(1)).0,
                new_id
            );
            assert_eq!(BACKUP_CADENCE.slot(earlier), BACKUP_CADENCE.slot(boundary));
            assert_eq!(BACKUP_CADENCE.slot(boundary).0, boundary.timestamp() / 3600);
        }
    }
}
