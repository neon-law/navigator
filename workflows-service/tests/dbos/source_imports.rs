//! `SourceImport` on DBOS: replicas join one slot and a stopped import is
//! reported without failing the run or re-running a checkpointed one.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use uuid::Uuid;
use workflows::dbos_engine;
use workflows::dbos_engine::source_imports::{
    self, Context, ImportQueueError, ImportReport, ImportSource, StoreImportSource,
};

use crate::support::{database, firm_matter, APP_VERSION, DEADLINE};

#[derive(Clone, Default)]
struct Queue {
    unreadable: bool,
    pending: Vec<Uuid>,
    refusing: Vec<Uuid>,
    applied: Arc<Mutex<BTreeMap<Uuid, usize>>>,
}

#[async_trait::async_trait]
impl ImportSource for Queue {
    async fn pending(&self) -> Result<Vec<Uuid>, ImportQueueError> {
        if self.unreadable {
            return Err(ImportQueueError::Queue);
        }
        Ok(self.pending.clone())
    }

    async fn apply(&self, import_id: Uuid) -> Result<(), String> {
        *self.applied.lock().unwrap().entry(import_id).or_default() += 1;
        if self.refusing.contains(&import_id) {
            return Err("repository already holds source that is not this import".into());
        }
        Ok(())
    }
}

#[tokio::test]
async fn replicas_join_one_import_slot_and_report_a_refusal() {
    let Some(db) = database().await else {
        return;
    };
    let (good, refused) = (Uuid::now_v7(), Uuid::now_v7());
    let queue = Queue {
        pending: vec![good, refused],
        refusing: vec![refused],
        ..Queue::default()
    };
    let slot = Utc.with_ymd_and_hms(2026, 10, 18, 9, 35, 0).unwrap();
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let a = source_imports::register(&first, Context::new(Arc::new(queue.clone()))).unwrap();
    let b = source_imports::register(&second, Context::new(Arc::new(queue.clone()))).unwrap();
    first.launch().await.unwrap();
    second.launch().await.unwrap();
    let (left, right) = tokio::join!(
        source_imports::fire(&a, slot),
        source_imports::fire(&b, slot)
    );
    left.unwrap();
    right.unwrap();
    let report: ImportReport = tokio::time::timeout(
        DEADLINE,
        first
            .retrieve_workflow::<ImportReport, ImportQueueError>(&source_imports::workflow_id(slot))
            .unwrap()
            .result(),
    )
    .await
    .unwrap()
    .unwrap();
    first.shutdown().await;
    second.shutdown().await;
    assert_eq!(report.committed, vec![good]);
    assert_eq!(report.stopped[0].0, refused);
    let applied = queue.applied.lock().unwrap();
    assert_eq!(applied.get(&good), Some(&1));
    assert_eq!(applied.get(&refused), Some(&1));
}

fn import_slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 9, 35, 0).unwrap()
}

async fn outcome_of(dbos: &dbos::DBOS) -> dbos::Result<ImportReport, ImportQueueError> {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<ImportReport, ImportQueueError>(&source_imports::workflow_id(
            import_slot(),
        ))
        .unwrap()
        .result(),
    )
    .await
    .expect("SourceImport did not finish")
}

#[tokio::test]
async fn the_schedule_applies_the_current_slot_and_stops_on_cancellation() {
    let Some(db) = database().await else {
        return;
    };
    let import = Uuid::now_v7();
    let queue = Queue {
        pending: vec![import],
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        source_imports::register(&worker, Context::new(Arc::new(queue.clone()))).unwrap();
    worker.launch().await.unwrap();
    let stop = dbos::CancellationToken::new();
    let mid_slot = import_slot() + chrono::TimeDelta::minutes(3);
    let scheduled = tokio::spawn(source_imports::schedule(
        workflow,
        Arc::new(move || mid_slot),
        stop.clone(),
    ));
    let report = outcome_of(&worker).await.expect("SourceImport succeeds");
    stop.cancel();
    tokio::time::timeout(DEADLINE, scheduled)
        .await
        .expect("schedule did not stop after cancellation")
        .expect("schedule panicked");
    worker.shutdown().await;
    assert_eq!(report.committed, vec![import]);
    assert!(report.stopped.is_empty());
}

#[tokio::test]
async fn an_unreadable_import_queue_fails_the_slot() {
    let Some(db) = database().await else {
        return;
    };
    let queue = Queue {
        unreadable: true,
        ..Queue::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        source_imports::register(&worker, Context::new(Arc::new(queue.clone()))).unwrap();
    worker.launch().await.unwrap();
    source_imports::fire(&workflow, import_slot())
        .await
        .unwrap();
    let outcome = outcome_of(&worker).await;
    worker.shutdown().await;
    assert!(outcome.is_err());
    assert!(queue.applied.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_store_source_reads_pending_imports_and_reports_why_one_stops() {
    let Some(db) = database().await else {
        return;
    };
    let pg = db.store().await;
    let surreal = store::test_support::mem_surreal().await;
    let matter = firm_matter(&surreal, "Import").await;
    let source = StoreImportSource::new(surreal, pg.clone(), Arc::new(cloud::NoRuntimeKms));
    assert!(source.pending().await.unwrap().is_empty());

    // An id the queue never held stops with a reason.
    assert!(source.apply(Uuid::now_v7()).await.is_err());

    // A recorded request on a Firm with no forge token stops before any
    // forge write, and stays pending for the next slot.
    let import = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO project_source_import \
         (id, firm_id, project_id, operation_key, tree_sha256, file_count, canonical_zip, \
          state, requested_by_person_id, requested_at) \
         VALUES ($1, $2, $3, 'scheduled-import', $4, 1, $5, 'pending', $6, now())",
    )
    .bind(import)
    .bind(matter.firm)
    .bind(matter.project)
    .bind("0".repeat(64))
    .bind(b"synthetic archive".to_vec())
    .bind(matter.admin)
    .execute(pg.pool())
    .await
    .expect("record a synthetic pending import");
    assert_eq!(source.pending().await.unwrap(), vec![import]);
    let reason = source
        .apply(import)
        .await
        .expect_err("no forge is configured for the Firm");
    assert!(!reason.is_empty());
    assert_eq!(source.pending().await.unwrap(), vec![import]);

    pg.pool().close().await;
    assert!(matches!(
        source.pending().await,
        Err(ImportQueueError::Queue)
    ));
}
