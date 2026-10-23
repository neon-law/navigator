//! `DriDigest` on DBOS: daily-slot deduplication, retries and checkpoint replay.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use tokio::sync::Notify;
use workflows::dbos_engine;
use workflows::dbos_engine::dri_digest::{
    self, Context, DriDigestError, DriDigestReport, DriDigestSource,
};
use workflows::{CapturingNotifier, Notifier, NotifyError};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone)]
struct FixedSource {
    projects: Vec<store::projects::ProjectDriSummary>,
    counts: (usize, usize),
}

#[async_trait::async_trait]
impl DriDigestSource for FixedSource {
    async fn projects(&self) -> Result<Vec<store::projects::ProjectDriSummary>, DriDigestError> {
        Ok(self.projects.clone())
    }

    async fn open_pitch_counts(&self) -> Result<(usize, usize), DriDigestError> {
        Ok(self.counts)
    }
}

#[derive(Clone, Default)]
struct HeldCounts {
    calls: Arc<AtomicUsize>,
    reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl DriDigestSource for HeldCounts {
    async fn projects(&self) -> Result<Vec<store::projects::ProjectDriSummary>, DriDigestError> {
        Ok(vec![project()])
    }

    async fn open_pitch_counts(&self) -> Result<(usize, usize), DriDigestError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok((3, 1))
    }
}

#[derive(Clone, Default)]
struct FailOnceNotifier {
    attempts: Arc<AtomicU32>,
    capture: CapturingNotifier,
}

#[async_trait::async_trait]
impl Notifier for FailOnceNotifier {
    async fn notify(&self, text: String) -> Result<(), NotifyError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(NotifyError::Transport("synthetic first failure".into()));
        }
        self.capture.notify(text).await
    }
}

fn project() -> store::projects::ProjectDriSummary {
    store::projects::ProjectDriSummary {
        code: "sample-litigation".into(),
        name: "Private project display name".into(),
        status: "open".into(),
        lawyer_dris: vec!["Synthetic Lawyer DRI".into()],
        client_dris: vec!["Synthetic Client DRI".into()],
    }
}

fn fixed_source() -> Arc<dyn DriDigestSource> {
    Arc::new(FixedSource {
        projects: vec![project()],
        counts: (3, 1),
    })
}

fn context(
    source: Arc<dyn DriDigestSource>,
    notifier: Arc<dyn Notifier>,
    simulated: bool,
) -> Context {
    Context::with_source(source, notifier, simulated)
}

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 13, 1, 11, 0).unwrap()
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> DriDigestReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<DriDigestReport, DriDigestError>(id)
            .expect("retrieve DriDigest")
            .result(),
    )
    .await
    .expect("DriDigest did not finish")
    .expect("DriDigest succeeds")
}

#[tokio::test]
async fn two_executors_join_the_same_daily_digest_and_keep_internal_fields_only() {
    let Some(db) = database().await else {
        return;
    };
    let first_notifier = CapturingNotifier::new();
    let second_notifier = CapturingNotifier::new();
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let second = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let first_workflow = dri_digest::register(
        &first,
        context(fixed_source(), Arc::new(first_notifier.clone()), true),
    )
    .expect("register first executor");
    let second_workflow = dri_digest::register(
        &second,
        context(fixed_source(), Arc::new(second_notifier.clone()), true),
    )
    .expect("register second executor");
    first.launch().await.expect("launch first executor");
    second.launch().await.expect("launch second executor");

    let (left, right) = tokio::join!(
        dri_digest::fire(&first_workflow, slot()),
        dri_digest::fire(&second_workflow, slot()),
    );
    left.expect("first start");
    right.expect("second start");
    let id = dri_digest::workflow_id(slot());
    assert_eq!(report_of(&first, &id).await.projects, 1);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    first.shutdown().await;
    second.shutdown().await;

    let mut notices = first_notifier.captured();
    notices.extend(second_notifier.captured());
    assert_eq!(notices.len(), 1, "one daily digest across executors");
    assert!(notices[0].contains("Synthetic Lawyer DRI"));
    assert!(notices[0].contains("Synthetic Client DRI"));
    assert!(!notices[0].contains("Private project display name"));
}

#[tokio::test]
async fn a_transient_notice_failure_retries_the_same_digest_step() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = FailOnceNotifier::default();
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dri_digest::register(
        &worker,
        context(fixed_source(), Arc::new(notifier.clone()), true),
    )
    .expect("register DriDigest");
    worker.launch().await.expect("launch");
    let id = dri_digest::workflow_id(slot());
    dri_digest::fire(&workflow, slot()).await.expect("start");
    let report = report_of(&worker, &id).await;
    worker.shutdown().await;

    assert_eq!(report.projects, 1);
    assert_eq!(notifier.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(notifier.capture.captured().len(), 1);
}

#[tokio::test]
async fn before_slot_a_stopped_schedule_does_not_start_the_daily_run() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = CapturingNotifier::new();
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dri_digest::register(&worker, context(fixed_source(), Arc::new(notifier), true))
        .expect("register DriDigest");
    worker.launch().await.expect("launch");

    let stop = dbos::CancellationToken::new();
    stop.cancel();
    let before_slot = Utc.with_ymd_and_hms(2026, 10, 13, 0, 0, 0).unwrap();
    tokio::time::timeout(
        DEADLINE,
        dri_digest::schedule(workflow, Arc::new(move || before_slot), stop),
    )
    .await
    .expect("schedule did not stop on cancellation");

    let started: i64 =
        sqlx::query_scalar("SELECT count(*) FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(dri_digest::workflow_id(slot()))
            .fetch_one(&db.pool)
            .await
            .expect("check the current slot");
    worker.shutdown().await;
    assert_eq!(
        started, 0,
        "a stopped pre-slot schedule waits without firing"
    );
}

#[tokio::test]
async fn after_slot_the_schedule_starts_the_current_daily_run_and_stops_on_cancellation() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = CapturingNotifier::new();
    let worker = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dri_digest::register(
        &worker,
        context(fixed_source(), Arc::new(notifier.clone()), true),
    )
    .expect("register DriDigest");
    worker.launch().await.expect("launch");

    let stop = dbos::CancellationToken::new();
    let scheduled = tokio::spawn(dri_digest::schedule(workflow, Arc::new(slot), stop.clone()));
    let id = dri_digest::workflow_id(slot());
    assert_eq!(report_of(&worker, &id).await.projects, 1);
    stop.cancel();
    tokio::time::timeout(DEADLINE, scheduled)
        .await
        .expect("schedule did not stop after cancellation")
        .expect("schedule panicked");
    worker.shutdown().await;
    assert_eq!(notifier.captured().len(), 1);
}

#[tokio::test]
async fn recovery_reuses_the_project_and_digest_checkpoints_before_counts() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = CapturingNotifier::new();
    let held = HeldCounts::default();
    let id = dri_digest::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dri_digest::register(
        &first,
        context(Arc::new(held.clone()), Arc::new(notifier.clone()), false),
    )
    .expect("register DriDigest");
    first.launch().await.expect("launch first executor");
    dri_digest::fire(&workflow, slot()).await.expect("start");
    tokio::time::timeout(DEADLINE, held.reached.notified())
        .await
        .expect("DriDigest did not reach its counts query");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read DriDigest checkpoints");
    assert_eq!(steps, ["query", "notify"]);

    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let source = Arc::new(FixedSource {
        projects: vec![project()],
        counts: (3, 1),
    });
    let workflow = dri_digest::register(
        &recovered,
        context(source, Arc::new(notifier.clone()), false),
    )
    .expect("register recovery executor");
    recovered.launch().await.expect("relaunch same version");
    dri_digest::fire(&workflow, slot())
        .await
        .expect("join recovered run");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert_eq!(report.projects, 1);
    let notices = notifier.captured();
    assert_eq!(notices.len(), 2, "one digest and one counts follow-up");
    assert!(notices[0].contains("Project DRI digest"));
    assert_eq!(notices[1], "Total open matters: 3, pitches: 1");
}

#[tokio::test]
async fn enabled_digest_schedule_registers_and_stops_with_the_dbos_executor() {
    let Some(db) = database().await else {
        return;
    };
    let settings = dbos_engine::Settings::from_lookup(db.lookup()).expect("complete settings");
    let notifier = CapturingNotifier::new();
    let now = Utc::now();
    let scheduled_slot = dri_digest::slot(now);
    let schedule_is_due = now >= scheduled_slot;
    let executor = dbos_engine::Executor::launch_with_schedules(
        &settings,
        Arc::new(notifier.clone()),
        None,
        Some(context(fixed_source(), Arc::new(notifier.clone()), true)),
    )
    .await
    .expect("register and launch enabled DriDigest schedule");

    if schedule_is_due {
        let id = dri_digest::workflow_id(scheduled_slot);
        tokio::time::timeout(DEADLINE, async {
            loop {
                let status = sqlx::query_scalar::<_, String>(
                    "SELECT status FROM dbos.workflow_status WHERE workflow_uuid = $1",
                )
                .bind(&id)
                .fetch_optional(&db.pool)
                .await
                .expect("read scheduled DriDigest status");
                if status.as_deref() == Some("SUCCESS") {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("enabled executor did not start its due DriDigest schedule");
        let notices = notifier.captured();
        assert_eq!(
            notices
                .iter()
                .filter(|notice| notice.starts_with("*Project DRI digest"))
                .count(),
            1,
            "the configured DriDigest schedule posts exactly one digest"
        );
    }

    tokio::time::timeout(DEADLINE, executor.shutdown())
        .await
        .expect("DBOS executor shutdown was not bounded");
}
