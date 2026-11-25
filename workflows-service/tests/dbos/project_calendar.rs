//! A provider outage can outlive the submitting replica. The retry reads
//! current assignments, and the same slot does not execute twice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use store::project_calendar::ParticipantSync;
use uuid::Uuid;
use workflows::dbos_engine::project_calendar::{register, Source, SyncError};
use workflows::dbos_engine::{Executor, Schedules, Settings};
use workflows::CapturingNotifier;

use crate::support::{database, APP_VERSION, DEADLINE};

struct CalendarSource {
    before: Uuid,
    after: Uuid,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Source for CalendarSource {
    async fn candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        Ok(Vec::new())
    }

    async fn backup(&self, _: Uuid) -> Result<u64, SyncError> {
        Ok(0)
    }

    async fn booking_candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        Ok(Vec::new())
    }

    async fn sync_booking(&self, _: Uuid) -> Result<u64, SyncError> {
        Ok(0)
    }

    async fn sync(&self, project_id: Uuid) -> Result<Vec<ParticipantSync>, SyncError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            assert_eq!(project_id, self.before);
            return Err(SyncError {
                phase: "provider_unavailable".into(),
                status: Some(503),
                person_id: None,
                attempts: 0,
            });
        }
        Ok(vec![ParticipantSync {
            person_id: self.after,
            access: "verified".into(),
        }])
    }
}

#[tokio::test]
async fn duplicate_calendar_jobs_share_the_retry_and_read_back_current_assignments() {
    let Some(db) = database().await else {
        return;
    };
    let source = Arc::new(CalendarSource {
        before: Uuid::now_v7(),
        after: Uuid::now_v7(),
        calls: AtomicUsize::new(0),
    });
    let runtime = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = register(&runtime, source.clone()).unwrap();
    runtime.launch().await.unwrap();
    let slot = "ProjectCalendarMembership:synthetic:1";
    let options = || dbos::StartOptions {
        workflow_id: Some(slot),
        ..Default::default()
    };
    let (one, two) = tokio::join!(
        workflow.start_with(source.before, options()),
        workflow.start_with(source.before, options()),
    );
    let one = one.unwrap();
    let two = two.unwrap();
    let result = tokio::time::timeout(DEADLINE, one.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.attempts, 2);
    assert_eq!(result.participants[0].person_id, source.after);
    assert_eq!(two.result().await.unwrap().attempts, 2);
    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    runtime.shutdown().await;
    db.close().await;
}

struct TwoProjects {
    denied: Uuid,
    allowed: Uuid,
    attorney: Uuid,
}

#[async_trait::async_trait]
impl Source for TwoProjects {
    async fn candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        Ok(vec![self.denied, self.allowed])
    }

    async fn backup(&self, _: Uuid) -> Result<u64, SyncError> {
        Ok(0)
    }

    async fn booking_candidates(&self) -> Result<Vec<Uuid>, SyncError> {
        Ok(vec![self.attorney])
    }

    async fn sync_booking(&self, attorney: Uuid) -> Result<u64, SyncError> {
        assert_eq!(attorney, self.attorney);
        Ok(1)
    }

    async fn sync(&self, project: Uuid) -> Result<Vec<ParticipantSync>, SyncError> {
        if project == self.denied {
            return Err(SyncError {
                phase: "provider_denied".into(),
                status: Some(403),
                person_id: None,
                attempts: 0,
            });
        }
        Ok(Vec::new())
    }
}

#[tokio::test]
async fn calendar_host_catches_up_its_schedules_and_isolates_a_denied_project() {
    let Some(db) = database().await else {
        return;
    };
    let source = Arc::new(TwoProjects {
        denied: Uuid::now_v7(),
        allowed: Uuid::now_v7(),
        attorney: Uuid::now_v7(),
    });
    let executor = Executor::launch_hosting(
        &Settings::from_lookup(db.lookup()).unwrap(),
        Arc::new(CapturingNotifier::new()),
        Schedules {
            project_calendar: Some(source.clone()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let store = db.store();
    for (name, project, status) in [
        ("ProjectCalendarMembership", source.denied, "ERROR"),
        ("ProjectCalendarMembership", source.allowed, "SUCCESS"),
        ("ProjectCalendarEventBackup", source.denied, "SUCCESS"),
        ("ProjectCalendarEventBackup", source.allowed, "SUCCESS"),
        ("AttorneyBookingAvailability", source.attorney, "SUCCESS"),
    ] {
        let prefix = format!("{name}:{project}:%");
        tokio::time::timeout(DEADLINE, async {
            loop {
                let recorded: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM dbos.workflow_status WHERE workflow_uuid LIKE $1 AND status = $2)",
                )
                .bind(&prefix)
                .bind(status)
                .fetch_one(store.pool())
                .await
                .unwrap();
                if recorded {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{prefix} did not reach {status}"));
    }
    executor.shutdown().await;
    db.close().await;
}
