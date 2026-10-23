//! `Heartbeat` on DBOS: registration, launch, a run, slot dedupe, recovery,
//! and the worker lifecycle.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::Notify;
use workflows::dbos_engine::heartbeat::{self, Clock, HeartbeatError};
use workflows::dbos_engine::{self, Settings};
use workflows::heartbeat::{heartbeat_message, HeartbeatReport};
use workflows::{CapturingNotifier, Notifier, NotifyError};

use crate::support::{database, APP_VERSION, DEADLINE};

fn beat_instant() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 13, 6, 0, 1).unwrap()
}

/// A fixed clock that counts its reads: one read per `beat` step body.
fn counting_clock() -> (Clock, Arc<AtomicU32>) {
    let reads = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&reads);
    let clock: Clock = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        beat_instant()
    });
    (clock, reads)
}

async fn report_of(dbos: &dbos::DBOS, workflow_id: &str) -> HeartbeatReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<HeartbeatReport, HeartbeatError>(workflow_id)
            .expect("retrieve the heartbeat")
            .result(),
    )
    .await
    .expect("the heartbeat never finished")
    .expect("the heartbeat succeeds")
}

#[tokio::test]
async fn a_slot_runs_once_and_posts_one_line() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = CapturingNotifier::new();
    let (clock, reads) = counting_clock();
    let dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = heartbeat::register(&dbos, Arc::new(notifier.clone()), clock).expect("register");
    dbos.launch().await.expect("launch");

    let slot = heartbeat::slot(beat_instant());
    let id = heartbeat::workflow_id(slot);
    heartbeat::fire(&workflow, slot).await.expect("start");
    let report = report_of(&dbos, &id).await;
    assert_eq!(report.invocation_id, id);
    assert_eq!(report.beat_at, beat_instant());
    assert_eq!(notifier.captured(), vec![heartbeat_message(&report)]);

    // A second start for the same slot joins the finished run.
    heartbeat::fire(&workflow, slot).await.expect("restart");
    assert_eq!(report_of(&dbos, &id).await, report);
    assert_eq!(notifier.captured().len(), 1, "one line per slot");
    assert_eq!(reads.load(Ordering::SeqCst), 1, "one beat per slot");
    dbos.shutdown().await;
}

/// Holds the first `notify` call before it posts, so shutdown abandons the
/// workflow after `beat` checkpointed; later calls post.
#[derive(Clone, Default)]
struct HeldNotifier {
    calls: Arc<AtomicU32>,
    posted: Arc<Mutex<Vec<String>>>,
    reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl Notifier for HeldNotifier {
    async fn notify(&self, text: String) -> Result<(), NotifyError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        self.posted.lock().expect("posted lock").push(text);
        Ok(())
    }
}

#[tokio::test]
async fn recovery_resumes_at_notify_without_a_second_beat() {
    let Some(db) = database().await else {
        return;
    };
    let notifier = HeldNotifier::default();
    let (clock, reads) = counting_clock();
    let dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = heartbeat::register(&dbos, Arc::new(notifier.clone()), clock).expect("register");
    dbos.launch().await.expect("launch");

    let slot = heartbeat::slot(beat_instant());
    let id = heartbeat::workflow_id(slot);
    heartbeat::fire(&workflow, slot).await.expect("start");
    tokio::time::timeout(DEADLINE, notifier.reached.notified())
        .await
        .expect("the heartbeat never reached notify");
    dbos.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read checkpoints");
    assert_eq!(steps, ["beat"], "only beat checkpointed before shutdown");

    dbos.launch()
        .await
        .expect("relaunch recovers the heartbeat");
    let report = report_of(&dbos, &id).await;
    dbos.shutdown().await;

    assert_eq!(reads.load(Ordering::SeqCst), 1, "beat replayed, not re-run");
    assert_eq!(
        notifier.posted.lock().expect("posted lock").clone(),
        vec![heartbeat_message(&report)]
    );
}

#[tokio::test]
async fn the_worker_fires_the_current_slot_on_launch() {
    let Some(db) = database().await else {
        return;
    };
    let settings = Settings::from_lookup(db.lookup()).expect("complete settings");
    let notifier = CapturingNotifier::new();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let worker = {
        let notifier: Arc<dyn Notifier> = Arc::new(notifier.clone());
        tokio::spawn(async move {
            dbos_engine::serve(&settings, notifier, async {
                let _ = stopped.await;
            })
            .await
        })
    };

    // The line posts inside `notify` before its checkpoint commits, so wait for
    // the run to finish rather than for the line.
    tokio::time::timeout(DEADLINE, async {
        loop {
            let finished: Option<bool> = sqlx::query_scalar(
                "SELECT bool_and(status = 'SUCCESS') FROM dbos.workflow_status WHERE name = $1",
            )
            .bind(heartbeat::WORKFLOW)
            .fetch_one(&db.pool)
            .await
            .ok()
            .flatten();
            if finished == Some(true) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the worker never finished a heartbeat");
    stop.send(()).expect("the worker is running");
    worker
        .await
        .expect("the worker panicked")
        .expect("the worker shuts down cleanly");

    let [line] = &notifier.captured()[..] else {
        panic!("expected one heartbeat line");
    };
    assert!(line.contains("Durable execution OK"), "{line}");
    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE name = $1")
            .bind(heartbeat::WORKFLOW)
            .fetch_all(&db.pool)
            .await
            .expect("read the heartbeat runs");
    assert_eq!(statuses, ["SUCCESS"], "one run, for the current slot");
}
