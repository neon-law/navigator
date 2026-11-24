//! One slot checks every invoiced Purchase once, however many replicas fire
//! it, and a Purchase that cannot be checked never stops the ones behind it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use uuid::Uuid;
use workflows::dbos_engine::purchase_activation::{
    fire, register, schedule, slot, system_clock, ActivationError, ActivationSource, Context,
};

use crate::support::{database, APP_VERSION, DEADLINE};

/// The queue's Purchases in order: paid, still unpaid, and unreadable.
struct Purchases {
    paid: Uuid,
    unpaid: Uuid,
    unreadable: Uuid,
    queue_reads: AtomicUsize,
    checked: Mutex<Vec<Uuid>>,
}

impl Purchases {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            paid: Uuid::now_v7(),
            unpaid: Uuid::now_v7(),
            unreadable: Uuid::now_v7(),
            queue_reads: AtomicUsize::new(0),
            checked: Mutex::default(),
        })
    }
}

#[async_trait::async_trait]
impl ActivationSource for Purchases {
    async fn invoiced(&self) -> Result<Vec<Uuid>, ActivationError> {
        self.queue_reads.fetch_add(1, Ordering::SeqCst);
        Ok(vec![self.unreadable, self.unpaid, self.paid])
    }

    async fn activate(&self, purchase_id: Uuid) -> Result<bool, String> {
        self.checked.lock().unwrap().push(purchase_id);
        if purchase_id == self.unreadable {
            return Err("invoice_unreadable".into());
        }
        Ok(purchase_id == self.paid)
    }
}

#[tokio::test]
async fn a_slot_fired_twice_checks_each_invoiced_purchase_once() {
    let Some(db) = database().await else {
        return;
    };
    let purchases = Purchases::new();
    let runtime = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = register(&runtime, Context::new(purchases.clone())).unwrap();
    runtime.launch().await.unwrap();

    // Two replicas inside one five-minute slot join the same run.
    let first = Utc.with_ymd_and_hms(2026, 11, 8, 12, 5, 0).unwrap();
    let second = Utc.with_ymd_and_hms(2026, 11, 8, 12, 9, 59).unwrap();
    assert_eq!(slot(first), slot(second));
    let one = fire(&workflow, slot(first)).await.unwrap();
    let two = fire(&workflow, slot(second)).await.unwrap();
    let report = tokio::time::timeout(DEADLINE, one.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(two.result().await.unwrap(), report);

    assert_eq!(report.activated, [purchases.paid]);
    assert_eq!(report.unpaid, [purchases.unpaid]);
    assert_eq!(
        report.stopped,
        [(purchases.unreadable, "invoice_unreadable".to_owned())]
    );
    assert_eq!(purchases.queue_reads.load(Ordering::SeqCst), 1);
    assert_eq!(
        *purchases.checked.lock().unwrap(),
        [purchases.unreadable, purchases.unpaid, purchases.paid]
    );

    // The next slot is a new run.
    let next = fire(&workflow, slot(second + chrono::TimeDelta::seconds(1)))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, next.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(purchases.queue_reads.load(Ordering::SeqCst), 2);
    runtime.shutdown().await;
}

#[tokio::test]
async fn the_schedule_fires_the_current_slot_and_stops_when_cancelled() {
    let Some(db) = database().await else {
        return;
    };
    let purchases = Purchases::new();
    let runtime = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = register(&runtime, Context::new(purchases.clone())).unwrap();
    runtime.launch().await.unwrap();

    let stop = dbos::CancellationToken::new();
    let running = tokio::spawn(schedule(workflow, system_clock(), stop.clone()));
    tokio::time::timeout(DEADLINE, async {
        while purchases.checked.lock().unwrap().len() < 3 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the schedule ran its first slot");
    stop.cancel();
    tokio::time::timeout(DEADLINE, running)
        .await
        .expect("the schedule stops on cancellation")
        .unwrap();
    assert_eq!(purchases.queue_reads.load(Ordering::SeqCst), 1);
    runtime.shutdown().await;
}
