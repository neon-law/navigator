//! A provider outage can outlive the submitting replica. The retry reads
//! current assignments, and the same slot does not execute twice.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use store::project_calendar::ParticipantSync;
use uuid::Uuid;
use workflows::dbos_engine::project_calendar::{register, Source, SyncError};

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
            calendar_invitation: "notification_requested".into(),
            event_responses: std::collections::BTreeMap::new(),
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
}
