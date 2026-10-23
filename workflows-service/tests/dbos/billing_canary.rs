//! `BillingCanary` on DBOS: weekly slot deduplication and recovery boundaries.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use billing::{ContactId, ContactRequest};
use chrono::{DateTime, TimeZone, Utc};
use tokio::sync::Notify;
use workflows::billing_canary::{BillingCanaryProvider, CanaryProviderError};
use workflows::dbos_engine;
use workflows::dbos_engine::billing_canary::{self, BillingCanaryError, Context, ENABLED_ENV};
use workflows::{CapturingEmail, EmailError, EmailService, OutboundEmail, SendReceipt};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone, Default)]
struct StableContactProvider {
    calls: Arc<AtomicUsize>,
    contacts: Arc<Mutex<HashMap<String, ContactId>>>,
}

#[async_trait::async_trait]
impl BillingCanaryProvider for StableContactProvider {
    async fn ensure_contact(
        &self,
        request: &ContactRequest,
    ) -> Result<ContactId, CanaryProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut contacts = self.contacts.lock().expect("contact map lock");
        Ok(contacts
            .entry(request.email.clone())
            .or_insert_with(|| ContactId("synthetic-contact".to_owned()))
            .clone())
    }
}

#[derive(Clone, Default)]
struct HeldProvider {
    provider: StableContactProvider,
    hold_first_attempt: Arc<AtomicUsize>,
    reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl BillingCanaryProvider for HeldProvider {
    async fn ensure_contact(
        &self,
        request: &ContactRequest,
    ) -> Result<ContactId, CanaryProviderError> {
        let contact = self.provider.ensure_contact(request).await?;
        if self.hold_first_attempt.fetch_add(1, Ordering::SeqCst) == 1 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(contact)
    }
}

#[derive(Clone, Default)]
struct AcceptedThenHeldEmail {
    capture: CapturingEmail,
    attempts: Arc<AtomicUsize>,
    reached: Arc<Notify>,
}

#[async_trait::async_trait]
impl EmailService for AcceptedThenHeldEmail {
    async fn send(&self, email: OutboundEmail) -> Result<SendReceipt, EmailError> {
        self.capture.send(email).await?;
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.reached.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(SendReceipt { message_id: None })
    }
}

fn slot() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 14, 0, 0).unwrap()
}

fn context(provider: Arc<dyn BillingCanaryProvider>, email: Arc<dyn EmailService>) -> Context {
    Context::new(provider, email)
}

async fn report_of(dbos: &dbos::DBOS, id: &str) -> billing_canary::CanaryReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<billing_canary::CanaryReport, BillingCanaryError>(id)
            .expect("retrieve BillingCanary")
            .result(),
    )
    .await
    .expect("BillingCanary did not finish")
    .expect("BillingCanary succeeds")
}

#[tokio::test]
async fn two_executors_join_the_same_weekly_run() {
    let Some(db) = database().await else {
        return;
    };
    let provider = StableContactProvider::default();
    let first_email = CapturingEmail::default();
    let second_email = CapturingEmail::default();
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "billing-canary-pod-a",
    ));
    let second = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "billing-canary-pod-b",
    ));
    let first_workflow = billing_canary::register(
        &first,
        context(Arc::new(provider.clone()), Arc::new(first_email.clone())),
    )
    .expect("register first executor");
    let second_workflow = billing_canary::register(
        &second,
        context(Arc::new(provider.clone()), Arc::new(second_email.clone())),
    )
    .expect("register second executor");
    first.launch().await.expect("launch first executor");
    second.launch().await.expect("launch second executor");

    let (left, right) = tokio::join!(
        billing_canary::fire(&first_workflow, slot()),
        billing_canary::fire(&second_workflow, slot()),
    );
    left.expect("first start");
    right.expect("second start");

    let id = billing_canary::workflow_id(slot());
    let report = report_of(&first, &id).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    first.shutdown().await;
    second.shutdown().await;

    assert!(report.idempotent);
    assert_eq!(report.contact_id, "synthetic-contact");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        first_email.captured().len() + second_email.captured().len(),
        1
    );
}

#[tokio::test]
async fn the_weekly_schedule_starts_its_current_slot_and_stops_on_cancellation() {
    let Some(db) = database().await else {
        return;
    };
    let provider = StableContactProvider::default();
    let email = CapturingEmail::default();
    let dbos = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_canary::register(
        &dbos,
        context(Arc::new(provider.clone()), Arc::new(email.clone())),
    )
    .expect("register the workflow");
    dbos.launch().await.expect("launch the executor");

    let now = slot() + chrono::Duration::minutes(1);
    let clock: billing_canary::Clock = Arc::new(move || now);
    let stop = dbos::CancellationToken::new();
    let schedule = tokio::spawn(billing_canary::schedule(workflow, clock, stop.clone()));
    let report = report_of(&dbos, &billing_canary::workflow_id(slot())).await;
    stop.cancel();
    tokio::time::timeout(DEADLINE, schedule)
        .await
        .expect("schedule did not stop")
        .expect("schedule task joins");
    dbos.shutdown().await;

    assert!(report.idempotent);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(email.captured().len(), 1);
}

#[tokio::test]
async fn recovery_reuses_the_contact_checkpoint_after_notification_accepts_then_stalls() {
    let Some(db) = database().await else {
        return;
    };
    let provider = StableContactProvider::default();
    let email = AcceptedThenHeldEmail::default();
    let id = billing_canary::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_canary::register(
        &first,
        context(Arc::new(provider.clone()), Arc::new(email.clone())),
    )
    .expect("register first executor");
    first.launch().await.expect("launch first executor");
    billing_canary::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, email.reached.notified())
        .await
        .expect("notification was not accepted");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read BillingCanary checkpoints");
    assert_eq!(steps, ["xero"], "Xero checkpointed before email began");

    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_canary::register(
        &recovered,
        context(Arc::new(provider.clone()), Arc::new(email.clone())),
    )
    .expect("register recovery executor");
    recovered.launch().await.expect("relaunch same version");
    billing_canary::fire(&workflow, slot())
        .await
        .expect("join recovered weekly run");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert!(report.idempotent);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "Xero step reused");
    assert_eq!(email.attempts.load(Ordering::SeqCst), 2);
    assert_eq!(
        email.capture.captured().len(),
        2,
        "Slack delivery is at least once"
    );
}

#[tokio::test]
async fn a_provider_effect_before_its_checkpoint_repeats_with_the_same_contact_key() {
    let Some(db) = database().await else {
        return;
    };
    let provider = HeldProvider::default();
    let email = CapturingEmail::default();
    let id = billing_canary::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_canary::register(
        &first,
        context(Arc::new(provider.clone()), Arc::new(email.clone())),
    )
    .expect("register first executor");
    first.launch().await.expect("launch first executor");
    billing_canary::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, provider.reached.notified())
        .await
        .expect("provider effect did not reach the checkpoint boundary");
    first.shutdown().await;

    let steps = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 \
         ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read BillingCanary checkpoints");
    assert!(steps.is_empty(), "the provider step did not checkpoint");

    let recovered = dbos::DBOS::new(dbos_engine::config(&db.url, APP_VERSION));
    let workflow = billing_canary::register(
        &recovered,
        context(Arc::new(provider.clone()), Arc::new(email.clone())),
    )
    .expect("register recovery executor");
    recovered.launch().await.expect("relaunch same version");
    billing_canary::fire(&workflow, slot())
        .await
        .expect("join recovered weekly run");
    let report = report_of(&recovered, &id).await;
    recovered.shutdown().await;

    assert!(report.idempotent);
    assert_eq!(provider.provider.calls.load(Ordering::SeqCst), 4);
    assert_eq!(email.captured().len(), 1);
}

#[test]
fn the_schedule_flag_is_default_off_and_rejects_malformed_values() {
    assert!(!billing_canary::enabled(|_| None).expect("unset means off"));
    assert!(!billing_canary::enabled(|_| Some("false".into())).expect("false means off"));
    assert!(billing_canary::enabled(|_| Some("true".into())).expect("true opts in"));
    assert_eq!(
        billing_canary::enabled(|_| Some("yes".into())).unwrap_err(),
        dbos_engine::SettingsError::Invalid(ENABLED_ENV)
    );
}
