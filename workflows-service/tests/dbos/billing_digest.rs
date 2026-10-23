//! DBOS `BillingDigest` deduplication, checkpoint recovery, and no-op delivery.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use tokio::sync::Notify;
use workflows::billing_digest::{AccountCost, BillingDigestReport, DigestConfig, DigestOutcome};
use workflows::dbos_engine;
use workflows::dbos_engine::billing_digest::{
    self, BillingDigestError, BillingDigestSource, Context,
};
use workflows::{CapturingEmail, EmailError, EmailService, OutboundEmail, SendReceipt};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone)]
struct FakeSource {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl BillingDigestSource for FakeSource {
    async fn query(
        &self,
        _query: &workflows::billing_digest::QueryConfig,
        window_days: u32,
    ) -> Result<BillingDigestReport, BillingDigestError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(BillingDigestReport {
            window_days,
            as_of: Utc.with_ymd_and_hms(2026, 10, 21, 13, 0, 0).unwrap(),
            accounts: vec![AccountCost {
                account: "synthetic-account".into(),
                current: vec![],
                prior: vec![],
                current_by_project: vec![],
                prior_by_project: vec![],
            }],
        })
    }
}

#[derive(Clone, Default)]
struct AcceptedThenHeldEmail {
    capture: CapturingEmail,
    calls: Arc<AtomicUsize>,
    reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl EmailService for AcceptedThenHeldEmail {
    async fn send(&self, email: OutboundEmail) -> Result<SendReceipt, EmailError> {
        self.capture.send(email).await?;
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(SendReceipt { message_id: None })
    }
}

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 21, 13, 0, 0).unwrap()
}

fn config(configured: bool) -> DigestConfig {
    DigestConfig::from_env(|key| match (configured, key) {
        (true, "BILLING_EXPORT_TABLE") => Some("project.dataset.export".into()),
        (true, "BIGQUERY_PROJECT") => Some("synthetic-project".into()),
        (true, "BILLING_DIGEST_NOTIFY_EMAIL") => Some("billing@example.com".into()),
        _ => None,
    })
}

fn context(source: FakeSource, email: Arc<dyn EmailService>, configured: bool) -> Context {
    Context::with_adapters(Arc::new(source), email, config(configured))
}

async fn outcome_of(dbos: &dbos::DBOS, id: &str) -> DigestOutcome {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<DigestOutcome, BillingDigestError>(id)
            .expect("retrieve BillingDigest")
            .result(),
    )
    .await
    .expect("BillingDigest did not finish")
    .expect("BillingDigest succeeds")
}

#[tokio::test]
async fn two_executors_join_one_daily_run_and_send_one_notice() {
    let Some(db) = database().await else {
        return;
    };
    let source = FakeSource {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let first_email = CapturingEmail::default();
    let second_email = CapturingEmail::default();
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "billing-digest-pod-a",
    ));
    let second = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "billing-digest-pod-b",
    ));
    let first_workflow = billing_digest::register(
        &first,
        context(source.clone(), Arc::new(first_email.clone()), true),
    )
    .expect("register first");
    let second_workflow = billing_digest::register(
        &second,
        context(source.clone(), Arc::new(second_email.clone()), true),
    )
    .expect("register second");
    first.launch().await.expect("launch first");
    second.launch().await.expect("launch second");
    let (left, right) = tokio::join!(
        billing_digest::fire(&first_workflow, slot()),
        billing_digest::fire(&second_workflow, slot())
    );
    left.expect("first start");
    right.expect("second start");
    let id = billing_digest::workflow_id(slot());
    let outcome = outcome_of(&first, &id).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    first.shutdown().await;
    second.shutdown().await;
    assert!(outcome.sent);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        first_email.captured().len() + second_email.captured().len(),
        1
    );
}

#[tokio::test]
async fn relaunch_reuses_the_checkpointed_query_without_rescanning() {
    let Some(db) = database().await else {
        return;
    };
    let source = FakeSource {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let email = AcceptedThenHeldEmail::default();
    let id = billing_digest::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_digest::register(
        &first,
        context(source.clone(), Arc::new(email.clone()), true),
    )
    .expect("register first");
    first.launch().await.expect("launch first");
    billing_digest::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, email.reached.notified())
        .await
        .expect("email step reached");
    first.shutdown().await;
    let steps = sqlx::query_scalar::<_, String>("SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 ORDER BY function_id")
        .bind(&id).fetch_all(&db.pool).await.expect("read checkpoints");
    assert_eq!(steps, ["query"]);

    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    billing_digest::register(
        &recovered,
        context(source.clone(), Arc::new(email.clone()), true),
    )
    .expect("register recovered");
    recovered.launch().await.expect("relaunch executor");
    let outcome = outcome_of(&recovered, &id).await;
    recovered.shutdown().await;
    assert!(outcome.sent);
    assert_eq!(
        source.calls.load(Ordering::SeqCst),
        1,
        "query checkpoint reused"
    );
    assert_eq!(
        email.capture.captured().len(),
        2,
        "webhook delivery may repeat after acceptance-before-checkpoint"
    );
}

#[tokio::test]
async fn unconfigured_run_sends_nothing() {
    let Some(db) = database().await else {
        return;
    };
    let source = FakeSource {
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let email = CapturingEmail::default();
    let dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_digest::register(
        &dbos,
        context(source.clone(), Arc::new(email.clone()), false),
    )
    .expect("register");
    dbos.launch().await.expect("launch");
    billing_digest::fire(&workflow, slot())
        .await
        .expect("start");
    let outcome = outcome_of(&dbos, &billing_digest::workflow_id(slot())).await;
    dbos.shutdown().await;
    assert!(!outcome.sent);
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(email.captured().is_empty());
}
