//! `ProjectRecovery` on DBOS: slot deduplication across replicas, a worker
//! lost mid-batch, and stalled generations reported without failing the run.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use store::project_archives::coverage::{Coverage, CoverageCounts, CoverageSummary, Uncovered};
use tokio::sync::Notify;
use uuid::Uuid;
use workflows::dbos_engine;
use workflows::dbos_engine::project_recovery::{
    self, Context, RecoveryError, RecoveryReport, RecoverySource, StoreRecoverySource,
};

use crate::support::{database, firm_matter, APP_VERSION, DEADLINE};

/// A queue of generations with per-generation outcomes and an optional hold.
#[derive(Clone, Default)]
struct Queue {
    unreadable: bool,
    unfinished: Vec<Uuid>,
    failing: Vec<Uuid>,
    hold: Option<Uuid>,
    coverage: Option<Result<CoverageSummary, String>>,
    reached: Arc<Notify>,
    advanced: Arc<Mutex<BTreeMap<Uuid, usize>>>,
}

#[async_trait::async_trait]
impl RecoverySource for Queue {
    async fn unfinished(&self) -> Result<Vec<Uuid>, RecoveryError> {
        if self.unreadable {
            return Err(RecoveryError::Queue);
        }
        Ok(self.unfinished.clone())
    }

    async fn advance(&self, generation_id: Uuid) -> Result<(), String> {
        *self
            .advanced
            .lock()
            .unwrap()
            .entry(generation_id)
            .or_default() += 1;
        if self.hold == Some(generation_id) {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        if self.failing.contains(&generation_id) {
            return Err("source object for documents/synthetic is missing".into());
        }
        Ok(())
    }

    async fn coverage(&self) -> Result<Option<CoverageSummary>, String> {
        self.coverage.clone().transpose()
    }
}

impl Queue {
    fn count(&self, id: Uuid) -> usize {
        self.advanced.lock().unwrap().get(&id).copied().unwrap_or(0)
    }
}

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 9, 30, 0).unwrap()
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> RecoveryReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<RecoveryReport, RecoveryError>(id)
            .expect("retrieve ProjectRecovery")
            .result(),
    )
    .await
    .expect("ProjectRecovery did not finish")
    .expect("ProjectRecovery succeeds")
}

#[tokio::test]
async fn replicas_join_one_slot_and_a_stalled_generation_does_not_fail_the_run() {
    let Some(db) = database().await else {
        return;
    };
    let (good, bad) = (Uuid::now_v7(), Uuid::now_v7());
    let queue = Queue {
        unfinished: vec![bad, good],
        failing: vec![bad],
        ..Queue::default()
    };
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow = project_recovery::register(&first, Context::new(Arc::new(queue.clone())))
        .expect("register first executor");
    let second_workflow =
        project_recovery::register(&second, Context::new(Arc::new(queue.clone())))
            .expect("register second executor");
    first.launch().await.expect("launch first executor");
    second.launch().await.expect("launch second executor");
    let (left, right) = tokio::join!(
        project_recovery::fire(&first_workflow, slot()),
        project_recovery::fire(&second_workflow, slot()),
    );
    left.expect("first start");
    right.expect("second start");
    let report = report_of(&first, &project_recovery::workflow_id(slot())).await;
    first.shutdown().await;
    second.shutdown().await;

    assert_eq!(report.committed, vec![good]);
    assert_eq!(report.stalled.len(), 1);
    assert_eq!(report.stalled[0].0, bad);
    assert!(!report.stalled[0].1.contains("synthetic bytes"));
    assert_eq!(queue.count(good), 1, "one slot advances a generation once");
    assert_eq!(queue.count(bad), 1);
}

#[tokio::test]
async fn a_worker_lost_mid_batch_resumes_from_its_checkpoints() {
    let Some(db) = database().await else {
        return;
    };
    let (done, held) = (Uuid::now_v7(), Uuid::now_v7());
    let queue = Queue {
        unfinished: vec![done, held],
        hold: Some(held),
        ..Queue::default()
    };
    let id = project_recovery::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = project_recovery::register(&first, Context::new(Arc::new(queue.clone())))
        .expect("register");
    first.launch().await.expect("launch");
    project_recovery::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, queue.reached.notified())
        .await
        .expect("ProjectRecovery did not reach the held generation");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read ProjectRecovery checkpoints");
    assert_eq!(steps, ["unfinished".to_string(), format!("archive-{done}")]);

    let released = Queue {
        hold: None,
        ..queue.clone()
    };
    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    project_recovery::register(&recovered, Context::new(Arc::new(released)))
        .expect("register recovery executor");
    recovered.launch().await.expect("relaunch the same version");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert_eq!(report.committed, vec![done, held]);
    assert_eq!(
        queue.count(done),
        1,
        "the checkpointed generation is not re-run"
    );
    assert_eq!(
        queue.count(held),
        2,
        "the step cut off by the crash runs again"
    );
}

#[tokio::test]
async fn the_schedule_starts_the_current_slot_and_stops_on_cancellation() {
    let Some(db) = database().await else {
        return;
    };
    let generation = Uuid::now_v7();
    let queue = Queue {
        unfinished: vec![generation],
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = project_recovery::register(&worker, Context::new(Arc::new(queue.clone())))
        .expect("register");
    worker.launch().await.expect("launch");
    let stop = dbos::CancellationToken::new();
    let late_in_slot = slot() + chrono::TimeDelta::minutes(7);
    let scheduled = tokio::spawn(project_recovery::schedule(
        workflow,
        Arc::new(move || late_in_slot),
        stop.clone(),
    ));
    let report = report_of(&worker, &project_recovery::workflow_id(slot())).await;
    stop.cancel();
    tokio::time::timeout(DEADLINE, scheduled)
        .await
        .expect("schedule did not stop after cancellation")
        .expect("schedule panicked");
    worker.shutdown().await;
    assert_eq!(report.committed, vec![generation]);
    assert_eq!(report.soft_deleted.as_deref(), Some("not configured"));
    assert_eq!(report.coverage, None);
    assert_eq!(report.coverage_error, None);
    assert_eq!(queue.count(generation), 1);
}

#[tokio::test]
async fn every_slot_reports_coverage_and_a_coverage_failure_does_not_fail_it() {
    let Some(db) = database().await else {
        return;
    };
    let missing = Uuid::now_v7();
    let summary = CoverageSummary {
        counts: CoverageCounts {
            expected: 2,
            current: 1,
            missing: 1,
            pending: 1,
            ..CoverageCounts::default()
        },
        uncovered: vec![Uncovered {
            project_id: missing,
            coverage: Coverage::Missing,
            pending: true,
        }],
    };
    let reported = Queue {
        coverage: Some(Ok(summary.clone())),
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        project_recovery::register(&worker, Context::new(Arc::new(reported))).expect("register");
    worker.launch().await.expect("launch");
    project_recovery::fire(&workflow, slot())
        .await
        .expect("start");
    let report = report_of(&worker, &project_recovery::workflow_id(slot())).await;
    worker.shutdown().await;
    assert_eq!(report.coverage, Some(summary));
    assert_eq!(report.coverage_error, None);

    let later = slot() + chrono::TimeDelta::minutes(15);
    let failing = Queue {
        coverage: Some(Err("the Project inventory could not be read".into())),
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        project_recovery::register(&worker, Context::new(Arc::new(failing))).expect("register");
    worker.launch().await.expect("launch");
    project_recovery::fire(&workflow, later)
        .await
        .expect("start");
    let report = report_of(&worker, &project_recovery::workflow_id(later)).await;
    worker.shutdown().await;
    assert_eq!(report.coverage, None);
    assert_eq!(
        report.coverage_error.as_deref(),
        Some("the Project inventory could not be read")
    );
}

#[tokio::test]
async fn an_unreadable_queue_fails_the_slot_without_advancing_anything() {
    let Some(db) = database().await else {
        return;
    };
    let queue = Queue {
        unreadable: true,
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = project_recovery::register(&worker, Context::new(Arc::new(queue.clone())))
        .expect("register");
    worker.launch().await.expect("launch");
    project_recovery::fire(&workflow, slot())
        .await
        .expect("start");
    let outcome = tokio::time::timeout(
        DEADLINE,
        worker
            .retrieve_workflow::<RecoveryReport, RecoveryError>(&project_recovery::workflow_id(
                slot(),
            ))
            .expect("retrieve ProjectRecovery")
            .result(),
    )
    .await
    .expect("ProjectRecovery did not finish");
    worker.shutdown().await;
    assert!(outcome.is_err(), "an unreadable queue fails the slot");
    assert!(queue.advanced.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_store_source_drives_a_recorded_generation_to_commit() {
    let Some(db) = database().await else {
        return;
    };
    let pg = db.store().await;
    let surreal = store::test_support::mem_surreal().await;
    let matter = firm_matter(&surreal, "Recovery").await;
    let documents: Arc<dyn cloud::StorageService> =
        Arc::new(store::test_support::MemoryStorage::default());
    let archive: Arc<dyn cloud::StorageService> =
        Arc::new(store::test_support::MemoryStorage::default());
    let source = StoreRecoverySource::new(
        surreal.clone(),
        pg.clone(),
        Arc::clone(&documents),
        Arc::clone(&archive),
        None,
    );
    assert!(source
        .unfinished()
        .await
        .expect("read the queue")
        .is_empty());

    let authority = store::project_archives::authorize(
        &surreal,
        store::persons::Role::Admin,
        matter.admin,
        matter.firm,
    )
    .await
    .expect("the Firm admin may archive");
    let stores = store::project_archives::ArchiveStores {
        surreal: &surreal,
        pg: &pg,
        documents: &documents,
        archive: &archive,
        source_backups: None,
    };
    let generation = store::project_archives::request(
        &stores,
        &authority,
        matter.project,
        "scheduled-recovery",
        None,
        Utc::now(),
    )
    .await
    .expect("record a generation");
    assert_eq!(source.unfinished().await.unwrap(), vec![generation.id]);
    let before = source
        .coverage()
        .await
        .expect("read coverage")
        .expect("the store source judges the inventory");
    assert!(before.uncovered.contains(&Uncovered {
        project_id: matter.project,
        coverage: Coverage::Missing,
        pending: true,
    }));
    source
        .advance(generation.id)
        .await
        .expect("advance the generation to commit");
    assert!(source.unfinished().await.unwrap().is_empty());
    let committed = store::project_archives::latest_committed(&pg, matter.project)
        .await
        .unwrap()
        .expect("a committed generation");
    assert_eq!(committed.id, generation.id);
    let after = source.coverage().await.unwrap().unwrap();
    assert!(
        after
            .uncovered
            .iter()
            .all(|entry| entry.project_id != matter.project),
        "a freshly committed generation covers its Project"
    );

    // An unknown generation stops with a reason, never a panic.
    assert!(source.advance(Uuid::now_v7()).await.is_err());
    let summary = source
        .archive_soft_deleted()
        .await
        .expect("nothing soft-deleted is still a report");
    assert_eq!(
        summary,
        "archived 0, active copies removed 0, without Firm 0, stalled 0"
    );

    // A lost database is a queue error, not an empty queue.
    pg.pool().close().await;
    assert!(matches!(
        source.unfinished().await,
        Err(RecoveryError::Queue)
    ));
    assert!(source.coverage().await.is_err());
}
