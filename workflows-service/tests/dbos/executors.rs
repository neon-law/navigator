//! Several executors on one system database: the shape of `web` running
//! more than one replica, and of a replica restarting.
//!
//! Two executors that start the same slot run it once. A run that shutdown
//! cuts off stays pending until an executor of the same application version
//! launches; an executor of another version leaves it alone.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::Notify;
use workflows::dbos_engine::heartbeat::{self, Clock, HeartbeatError};
use workflows::dbos_engine::{self, Executor, Settings, RELEASE_ENV};
use workflows::heartbeat::HeartbeatReport;
use workflows::{CapturingNotifier, Notifier, NotifyError};

use crate::support::{database, TestDatabase, APP_VERSION, DEADLINE};

fn beat_instant() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 13, 12, 0, 1).unwrap()
}

/// A fixed clock that counts its reads: one read per `beat` step body.
fn counting_clock(reads: &Arc<AtomicU32>) -> Clock {
    let reads = Arc::clone(reads);
    Arc::new(move || {
        reads.fetch_add(1, Ordering::SeqCst);
        beat_instant()
    })
}

async fn statuses(db: &TestDatabase) -> Vec<(String, String)> {
    sqlx::query_as("SELECT workflow_uuid, status FROM dbos.workflow_status WHERE name = $1")
        .bind(heartbeat::WORKFLOW)
        .fetch_all(&db.pool)
        .await
        .expect("read the heartbeat runs")
}

async fn wait_for(what: &str, mut done: impl AsyncFnMut() -> bool) {
    tokio::time::timeout(DEADLINE, async {
        while !done().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

#[tokio::test]
async fn two_executors_run_one_slot_once() {
    let Some(db) = database().await else {
        return;
    };
    let reads = Arc::new(AtomicU32::new(0));
    let (first_notifier, second_notifier) = (CapturingNotifier::new(), CapturingNotifier::new());
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "navigator-pod-a",
    ));
    let second = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "navigator-pod-b",
    ));
    let first_beat = heartbeat::register(
        &first,
        Arc::new(first_notifier.clone()),
        counting_clock(&reads),
    )
    .expect("register on the first executor");
    let second_beat = heartbeat::register(
        &second,
        Arc::new(second_notifier.clone()),
        counting_clock(&reads),
    )
    .expect("register on the second executor");
    first.launch().await.expect("launch the first executor");
    second.launch().await.expect("launch the second executor");

    // Both replicas reach the same slot boundary at once.
    let slot = heartbeat::slot(beat_instant());
    let (a, b) = tokio::join!(
        heartbeat::fire(&first_beat, slot),
        heartbeat::fire(&second_beat, slot)
    );
    a.expect("the first start");
    b.expect("the second start");

    let id = heartbeat::workflow_id(slot);
    let report: HeartbeatReport = tokio::time::timeout(
        DEADLINE,
        first
            .retrieve_workflow::<HeartbeatReport, HeartbeatError>(&id)
            .expect("retrieve the heartbeat")
            .result(),
    )
    .await
    .expect("the heartbeat never finished")
    .expect("the heartbeat succeeds");
    assert_eq!(report.invocation_id, id);
    // A late duplicate would show up after the first run finished.
    tokio::time::sleep(Duration::from_millis(500)).await;
    first.shutdown().await;
    second.shutdown().await;

    assert_eq!(statuses(&db).await, [(id, "SUCCESS".to_owned())]);
    assert_eq!(reads.load(Ordering::SeqCst), 1, "one beat for the slot");
    let lines = first_notifier.captured().len() + second_notifier.captured().len();
    assert_eq!(lines, 1, "one line for the slot across both executors");
}

/// Holds the first `notify` call forever, so shutdown cuts the run off after
/// `beat` checkpointed.
#[derive(Clone, Default)]
struct HeldNotifier {
    calls: Arc<AtomicU32>,
    reached: Arc<Notify>,
    posted: Arc<Mutex<Vec<String>>>,
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

fn settings(db: &TestDatabase, version: &'static str, executor_id: &'static str) -> Settings {
    let lookup = db.lookup();
    Settings::from_lookup(move |key| {
        if key == RELEASE_ENV {
            Some(version.to_owned())
        } else if key == dbos_engine::EXECUTOR_ID_ENV {
            Some(executor_id.to_owned())
        } else {
            lookup(key)
        }
    })
    .expect("complete settings")
}

#[tokio::test]
async fn the_next_launch_of_the_same_version_recovers_a_cut_off_run() {
    let Some(db) = database().await else {
        return;
    };
    // The first process: its schedule starts the current slot on launch, and
    // shutdown cuts the run off inside `notify`.
    let held = HeldNotifier::default();
    let executor = Executor::launch(
        &settings(&db, APP_VERSION, "navigator-pod-a"),
        Arc::new(held.clone()),
    )
    .await
    .expect("launch the first executor");
    tokio::time::timeout(DEADLINE, held.reached.notified())
        .await
        .expect("the heartbeat never reached notify");

    // A live peer with a distinct identity cannot re-enqueue or take the
    // first pod's still-running step during launch recovery.
    let peer_notifier = CapturingNotifier::new();
    let peer = Executor::launch(
        &settings(&db, APP_VERSION, "navigator-pod-b"),
        Arc::new(peer_notifier.clone()),
    )
    .await
    .expect("launch a live peer");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let [(_, held_status)] = &statuses(&db).await[..] else {
        panic!("expected one heartbeat run while the first pod is live");
    };
    assert_eq!(
        held_status, "PENDING",
        "the live peer leaves the run pending"
    );
    assert!(
        peer_notifier.captured().is_empty(),
        "the live peer does not execute the first pod's held step"
    );
    peer.shutdown().await;
    executor.shutdown().await;
    let [(id, status)] = &statuses(&db).await[..] else {
        panic!("expected one heartbeat run");
    };
    assert_eq!(status, "PENDING", "shutdown leaves the run pending");

    // Another release leaves this version's run alone.
    let other = CapturingNotifier::new();
    let executor = Executor::launch(
        &settings(&db, "other", "navigator-pod-a"),
        Arc::new(other.clone()),
    )
    .await
    .expect("launch an executor of another version");
    tokio::time::sleep(Duration::from_secs(1)).await;
    executor.shutdown().await;
    let pending: String =
        sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid = $1")
            .bind(id)
            .fetch_one(&db.pool)
            .await
            .expect("read the cut-off run");
    assert_eq!(pending, "PENDING", "another version does not recover it");

    // A fresh process of the same version recovers it and posts once.
    let recovered = CapturingNotifier::new();
    // A replacement pod normally has a new name and cannot discover this
    // pending identity. The operator handoff supplies the recorded dead ID.
    let executor = Executor::launch(
        &settings(&db, APP_VERSION, "navigator-pod-a"),
        Arc::new(recovered.clone()),
    )
    .await
    .expect("relaunch the same version");
    wait_for("the recovered run to post", async || {
        !recovered.captured().is_empty()
    })
    .await;
    wait_for("the recovered run to finish", async || {
        let status: String =
            sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid = $1")
                .bind(id)
                .fetch_one(&db.pool)
                .await
                .expect("read the recovered run");
        status == "SUCCESS"
    })
    .await;
    executor.shutdown().await;

    assert_eq!(recovered.captured().len(), 1, "one line after recovery");
    assert!(held.posted.lock().expect("posted lock").is_empty());
    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(id)
    .fetch_all(&db.pool)
    .await
    .expect("read checkpoints");
    assert_eq!(steps, ["beat", "notify"], "beat replayed, notify completed");
}
