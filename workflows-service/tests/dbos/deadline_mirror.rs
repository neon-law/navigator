//! `DeadlineMirror` on DBOS: duplicate schedulers join one daily run, a
//! dry-run context plans without applying, and one matter's failure reads
//! as an outcome line instead of failing the night.

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use uuid::Uuid;
use workflows::dbos_engine;
use workflows::dbos_engine::deadline_mirror::{self, Context, MirrorQueueError, MirrorSource};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone)]
struct Matters {
    open: Vec<(Uuid, String)>,
    failed_project: Option<Uuid>,
    reconciled: Arc<Mutex<Vec<(Uuid, bool)>>>,
}

impl Matters {
    fn new(open: Vec<(Uuid, String)>) -> Self {
        Self {
            open,
            failed_project: None,
            reconciled: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait::async_trait]
impl MirrorSource for Matters {
    async fn open_projects(
        &self,
        _now: DateTime<Utc>,
    ) -> Result<Vec<(Uuid, String)>, MirrorQueueError> {
        Ok(self.open.clone())
    }

    async fn reconcile(
        &self,
        project_id: Uuid,
        dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<store::deadline_calendar::MirrorReport, String> {
        self.reconciled.lock().unwrap().push((project_id, dry_run));
        if self.failed_project == Some(project_id) {
            return Err("calendar unreachable".into());
        }
        let mut pass = store::deadline_calendar::MirrorReport::default();
        pass.outcome = store::deadline_calendar::MirrorOutcome::Mirrored;
        pass.dry_run = dry_run;
        pass.upserted = vec![Uuid::now_v7()];
        Ok(pass)
    }
}

fn slot() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 30, 4, 3, 0).unwrap()
}

async fn report_of(dbos: &dbos::DBOS) -> workflows::dbos_engine::deadline_mirror::MirrorRunReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<
            workflows::dbos_engine::deadline_mirror::MirrorRunReport,
            workflows::dbos_engine::deadline_mirror::MirrorQueueError,
        >(&deadline_mirror::workflow_id(slot()))
        .unwrap()
        .result(),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn duplicate_schedulers_join_one_daily_run() {
    let Some(db) = database().await else {
        return;
    };
    let matters = vec![
        (Uuid::now_v7(), "acme".to_string()),
        (Uuid::now_v7(), "bolts".to_string()),
    ];
    let source = Matters::new(matters.clone());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let a = deadline_mirror::register(&first, Context::new(Arc::new(source.clone()))).unwrap();
    let b = deadline_mirror::register(&second, Context::new(Arc::new(source.clone()))).unwrap();
    first.launch().await.unwrap();
    second.launch().await.unwrap();
    let (left, right) = tokio::join!(
        deadline_mirror::fire(&a, slot()),
        deadline_mirror::fire(&b, slot())
    );
    left.unwrap();
    right.unwrap();
    let report = report_of(&first).await;
    first.shutdown().await;
    second.shutdown().await;
    assert_eq!(report.passes.len(), 2);
    assert_eq!(
        report.passes[0].1, "acme",
        "passes carry the matter code beside its id"
    );
    assert_eq!(
        report.passes[0].2,
        "mirrored upserted=1 in_sync=0 deleted=0"
    );
    let reconciled = source.reconciled.lock().unwrap();
    for (project_id, _code) in &matters {
        assert_eq!(
            reconciled.iter().filter(|(id, _)| id == project_id).count(),
            1,
            "a checkpointed matter is not mirrored twice"
        );
    }
    assert!(reconciled.iter().all(|(_, dry_run)| !dry_run));
}

#[tokio::test]
async fn a_dry_run_context_rehearses_without_applying() {
    let Some(db) = database().await else {
        return;
    };
    let source = Matters::new(vec![(Uuid::now_v7(), "acme".to_string())]);
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow =
        deadline_mirror::register(&worker, Context::new(Arc::new(source.clone())).dry_run())
            .unwrap();
    worker.launch().await.unwrap();
    deadline_mirror::fire(&workflow, slot()).await.unwrap();
    let report = report_of(&worker).await;
    worker.shutdown().await;
    assert_eq!(
        report.passes[0].2,
        "mirrored upserted=1 in_sync=0 deleted=0 (dry run)"
    );
    assert!(
        source
            .reconciled
            .lock()
            .unwrap()
            .iter()
            .all(|(_, dry_run)| *dry_run),
        "the dry run reaches the source with dry_run set"
    );
}

#[tokio::test]
async fn one_matters_failure_reads_as_a_line_and_the_run_continues() {
    let Some(db) = database().await else {
        return;
    };
    let good = Uuid::now_v7();
    let bad = Uuid::now_v7();
    let source = Matters::new(vec![
        (good, "acme".to_string()),
        (bad, "unreachable".to_string()),
    ]);
    source.failed_project = Some(bad);
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = deadline_mirror::register(&worker, Context::new(Arc::new(source))).unwrap();
    worker.launch().await.unwrap();
    deadline_mirror::fire(&workflow, slot()).await.unwrap();
    let report = report_of(&worker).await;
    worker.shutdown().await;
    assert_eq!(report.passes.len(), 2, "the run continues past the failure");
    assert_eq!(
        report.passes[0].2,
        "mirrored upserted=1 in_sync=0 deleted=0"
    );
    assert_eq!(report.passes[1].2, "failed: calendar unreachable");
}
