//! `RetentionPurge` on DBOS: duplicate schedulers join one daily run, and a
//! worker lost mid-run resumes without purging a checkpointed chain again.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::Notify;
use uuid::Uuid;
use workflows::dbos_engine;
use workflows::dbos_engine::retention_purge::{
    self, Context, PurgeQueueError, PurgeRunReport, RetentionSource,
};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone, Default)]
struct Chains {
    unreadable: bool,
    due: Vec<(Uuid, String)>,
    hold: Option<String>,
    reached: Arc<Notify>,
    purged: Arc<Mutex<BTreeMap<String, usize>>>,
    judged_at: Arc<Mutex<Vec<DateTime<Utc>>>>,
}

#[async_trait::async_trait]
impl RetentionSource for Chains {
    async fn due(&self, now: DateTime<Utc>) -> Result<Vec<(Uuid, String)>, PurgeQueueError> {
        self.judged_at.lock().unwrap().push(now);
        if self.unreadable {
            return Err(PurgeQueueError::Queue);
        }
        Ok(self.due.clone())
    }

    async fn purge(&self, _project_id: Uuid, chain_key: &str, now: DateTime<Utc>) -> String {
        self.judged_at.lock().unwrap().push(now);
        *self
            .purged
            .lock()
            .unwrap()
            .entry(chain_key.to_string())
            .or_default() += 1;
        if self.hold.as_deref() == Some(chain_key) {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        "purged".into()
    }
}

fn slot() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 3, 17, 0).unwrap()
}

async fn report_of(dbos: &dbos::DBOS) -> PurgeRunReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<PurgeRunReport, PurgeQueueError>(&retention_purge::workflow_id(
            slot(),
        ))
        .unwrap()
        .result(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn duplicate_schedulers_join_one_daily_run_judged_at_the_slot() {
    let Some(db) = database().await else {
        return;
    };
    let project = Uuid::now_v7();
    let chains = Chains {
        due: vec![(project, "brief".into()), (project, "memo".into())],
        ..Chains::default()
    };
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let a = retention_purge::register(&first, Context::new(Arc::new(chains.clone()))).unwrap();
    let b = retention_purge::register(&second, Context::new(Arc::new(chains.clone()))).unwrap();
    first.launch().await.unwrap();
    second.launch().await.unwrap();
    let (left, right) = tokio::join!(
        retention_purge::fire(&a, slot()),
        retention_purge::fire(&b, slot())
    );
    left.unwrap();
    right.unwrap();
    let report = report_of(&first).await;
    first.shutdown().await;
    second.shutdown().await;
    assert_eq!(report.chains.len(), 2);
    assert!(chains
        .purged
        .lock()
        .unwrap()
        .values()
        .all(|count| *count == 1));
    assert!(chains
        .judged_at
        .lock()
        .unwrap()
        .iter()
        .all(|at| *at == slot()));
}

#[tokio::test]
async fn a_restart_resumes_after_the_last_checkpointed_chain() {
    let Some(db) = database().await else {
        return;
    };
    let project = Uuid::now_v7();
    let chains = Chains {
        due: vec![(project, "brief".into()), (project, "memo".into())],
        hold: Some("memo".into()),
        ..Chains::default()
    };
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        retention_purge::register(&first, Context::new(Arc::new(chains.clone()))).unwrap();
    first.launch().await.unwrap();
    retention_purge::fire(&workflow, slot()).await.unwrap();
    tokio::time::timeout(DEADLINE, chains.reached.notified())
        .await
        .unwrap();
    first.shutdown().await;

    let released = Chains {
        hold: None,
        ..chains.clone()
    };
    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    retention_purge::register(&recovered, Context::new(Arc::new(released))).unwrap();
    recovered.launch().await.unwrap();
    let report = report_of(&recovered).await;
    recovered.shutdown().await;
    assert_eq!(report.chains.len(), 2);
    let purged = chains.purged.lock().unwrap();
    assert_eq!(
        purged.get("brief"),
        Some(&1),
        "a checkpointed chain is not purged again"
    );
    assert_eq!(
        purged.get("memo"),
        Some(&2),
        "the interrupted step runs again"
    );
}

#[tokio::test]
async fn before_its_slot_a_stopped_schedule_waits_without_firing() {
    let Some(db) = database().await else {
        return;
    };
    let chains = Chains {
        due: vec![(Uuid::now_v7(), "brief".into())],
        ..Chains::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        retention_purge::register(&worker, Context::new(Arc::new(chains.clone()))).unwrap();
    worker.launch().await.unwrap();
    let stop = dbos::CancellationToken::new();
    stop.cancel();
    let before_slot = Utc.with_ymd_and_hms(2026, 10, 18, 1, 0, 0).unwrap();
    tokio::time::timeout(
        DEADLINE,
        retention_purge::schedule(workflow, Arc::new(move || before_slot), stop),
    )
    .await
    .expect("schedule did not stop on cancellation");
    let started: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(retention_purge::workflow_id(slot()))
            .fetch_one(&db.pool)
            .await
            .unwrap();
    worker.shutdown().await;
    assert_eq!(started, 0);
    assert!(chains.purged.lock().unwrap().is_empty());
}

#[tokio::test]
async fn after_its_slot_the_schedule_runs_the_day_and_stops_on_cancellation() {
    let Some(db) = database().await else {
        return;
    };
    let project = Uuid::now_v7();
    let chains = Chains {
        due: vec![(project, "brief".into())],
        ..Chains::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        retention_purge::register(&worker, Context::new(Arc::new(chains.clone()))).unwrap();
    worker.launch().await.unwrap();
    let stop = dbos::CancellationToken::new();
    let afternoon = Utc.with_ymd_and_hms(2026, 10, 18, 15, 0, 0).unwrap();
    let scheduled = tokio::spawn(retention_purge::schedule(
        workflow,
        Arc::new(move || afternoon),
        stop.clone(),
    ));
    let report = report_of(&worker).await;
    stop.cancel();
    tokio::time::timeout(DEADLINE, scheduled)
        .await
        .expect("schedule did not stop after cancellation")
        .expect("schedule panicked");
    worker.shutdown().await;
    assert_eq!(
        report.chains,
        vec![(project, "brief".to_string(), "purged".to_string())]
    );
    assert!(
        chains
            .judged_at
            .lock()
            .unwrap()
            .iter()
            .all(|at| *at == slot()),
        "a late start still judges the window at the slot"
    );
}

#[tokio::test]
async fn an_unreadable_due_list_fails_the_day_without_purging() {
    let Some(db) = database().await else {
        return;
    };
    let chains = Chains {
        unreadable: true,
        ..Chains::default()
    };
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        retention_purge::register(&worker, Context::new(Arc::new(chains.clone()))).unwrap();
    worker.launch().await.unwrap();
    retention_purge::fire(&workflow, slot()).await.unwrap();
    let outcome = tokio::time::timeout(
        DEADLINE,
        worker
            .retrieve_workflow::<PurgeRunReport, PurgeQueueError>(&retention_purge::workflow_id(
                slot(),
            ))
            .unwrap()
            .result(),
    )
    .await
    .expect("RetentionPurge did not finish");
    worker.shutdown().await;
    assert!(outcome.is_err());
    assert!(chains.purged.lock().unwrap().is_empty());
}
