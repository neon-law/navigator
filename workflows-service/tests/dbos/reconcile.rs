//! `ReconcileInvoices` DBOS identity and shared-executor behavior against real
//! PostgreSQL system tables, using synthetic providers and Slack.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use billing::{
    ReceivableInvoice, StubBillingProvider, TrustBankAccount, TrustBankTransaction,
    TrustTransactionKind,
};
use billing_workflows::ReconcileReport;
use chrono::{TimeZone, Utc};
use tokio::sync::Notify;
use workflows::dbos_engine;
use workflows::{CapturingSlackBot, SlackBot, SlackBotError, SlackChannel, SlackMessageReceipt};
use workflows_service::dbos_reconcile::{self, Context, ReconcileDbosError, ReconcileRunner};

use crate::support::{database, APP_VERSION, DEADLINE};

#[derive(Clone, Default)]
struct AcceptThenLoseReply {
    inner: CapturingSlackBot,
    attempts: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl SlackBot for AcceptThenLoseReply {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.inner.create_private_channel(name).await
    }

    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.inner.post_message(channel, text).await
    }

    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.inner.user_id_by_email(email).await
    }

    async fn channel_member_ids(&self, channel: &str) -> Result<Vec<String>, SlackBotError> {
        self.inner.channel_member_ids(channel).await
    }

    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        self.inner.invite_to_channel(channel, users).await
    }

    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        let receipt = self
            .inner
            .post_message_with_receipt(channel, text, correlation)
            .await?;
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(SlackBotError::Transport(
                "synthetic response lost after acceptance".into(),
            ));
        }
        Ok(receipt)
    }

    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.inner
            .message_receipt_by_correlation(channel, correlation)
            .await
    }
}

#[derive(Clone, Default)]
struct AcceptThenHold {
    inner: CapturingSlackBot,
    attempts: Arc<AtomicUsize>,
    accepted: Arc<Notify>,
}

#[async_trait::async_trait]
impl SlackBot for AcceptThenHold {
    async fn create_private_channel(&self, name: &str) -> Result<SlackChannel, SlackBotError> {
        self.inner.create_private_channel(name).await
    }
    async fn post_message(&self, channel: &str, text: &str) -> Result<(), SlackBotError> {
        self.inner.post_message(channel, text).await
    }
    async fn user_id_by_email(&self, email: &str) -> Result<Option<String>, SlackBotError> {
        self.inner.user_id_by_email(email).await
    }
    async fn channel_member_ids(&self, channel: &str) -> Result<Vec<String>, SlackBotError> {
        self.inner.channel_member_ids(channel).await
    }
    async fn invite_to_channel(
        &self,
        channel: &str,
        users: &[String],
    ) -> Result<(), SlackBotError> {
        self.inner.invite_to_channel(channel, users).await
    }
    async fn post_message_with_receipt(
        &self,
        channel: &str,
        text: &str,
        correlation: Option<&str>,
    ) -> Result<SlackMessageReceipt, SlackBotError> {
        let receipt = self
            .inner
            .post_message_with_receipt(channel, text, correlation)
            .await?;
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.accepted.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(receipt)
    }
    async fn message_receipt_by_correlation(
        &self,
        channel: &str,
        correlation: &str,
    ) -> Result<Option<SlackMessageReceipt>, SlackBotError> {
        self.inner
            .message_receipt_by_correlation(channel, correlation)
            .await
    }
}

#[derive(Clone)]
struct Runner(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ReconcileRunner for Runner {
    async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ReconcileReport {
            ingested: 2,
            ..Default::default()
        })
    }
}

struct ActualReconcileRunner {
    provider: Arc<StubBillingProvider>,
    surreal: store::surreal::SurrealDb,
}

struct HoldAfterMirror {
    actual: ActualReconcileRunner,
    calls: AtomicUsize,
    wrote: Notify,
}

#[async_trait::async_trait]
impl ReconcileRunner for HoldAfterMirror {
    async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError> {
        let report = self.actual.run().await?;
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            self.wrote.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(report)
    }
}

#[async_trait::async_trait]
impl ReconcileRunner for ActualReconcileRunner {
    async fn run(&self) -> Result<ReconcileReport, ReconcileDbosError> {
        billing_workflows::reconcile_once(self.provider.as_ref(), &self.surreal)
            .await
            .map_err(|_| ReconcileDbosError::Reconcile)
    }
}

fn slot() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 22, 11, 0, 0).unwrap()
}

fn context(runner: Runner, slack: Arc<dyn SlackBot>) -> Context {
    Context::with_runner(Arc::new(runner), slack, "synthetic-finance".into(), true)
}

async fn result(dbos: &dbos::DBOS, id: &str) -> ReconcileReport {
    tokio::time::timeout(
        DEADLINE,
        dbos.retrieve_workflow::<ReconcileReport, ReconcileDbosError>(id)
            .expect("retrieve ReconcileInvoices")
            .result(),
    )
    .await
    .expect("ReconcileInvoices did not finish")
    .expect("ReconcileInvoices succeeds")
}

async fn seed_trust_deposit(surreal: &store::surreal::SurrealDb) -> uuid::Uuid {
    let nevada = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new("Nevada", "NV", "state"),
    )
    .await
    .expect("seed synthetic jurisdiction")
    .id;
    let notation = store::test_support::seed_notation(surreal).await;
    let project_id = store::notations::find_by_id(surreal, notation)
        .await
        .expect("load synthetic notation")
        .expect("seeded notation")
        .project_id;
    store::projects::set_jurisdiction(surreal, project_id, Some(nevada))
        .await
        .expect("assign synthetic jurisdiction");
    project_id
}

#[tokio::test]
async fn two_executors_join_one_reconciliation_and_one_finance_notice() {
    let Some(db) = database().await else { return };
    let runner = Runner(Arc::new(AtomicUsize::new(0)));
    let first_slack = Arc::new(CapturingSlackBot::default());
    let second_slack = Arc::new(CapturingSlackBot::default());
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-pod-a",
    ));
    let second = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-pod-b",
    ));
    let left = dbos_reconcile::register(&first, context(runner.clone(), first_slack.clone()))
        .expect("register first executor");
    let right = dbos_reconcile::register(&second, context(runner.clone(), second_slack.clone()))
        .expect("register second executor");
    first.launch().await.expect("launch first");
    second.launch().await.expect("launch second");
    let (left, right) = tokio::join!(
        dbos_reconcile::fire(&left, slot()),
        dbos_reconcile::fire(&right, slot()),
    );
    left.expect("start first");
    right.expect("start second");
    let id = dbos_reconcile::workflow_id(slot());
    let report = result(&first, &id).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    first.shutdown().await;
    second.shutdown().await;

    assert_eq!(id, "ReconcileInvoices:2026-10-22");
    assert_eq!(report.ingested, 2);
    assert_eq!(runner.0.load(Ordering::SeqCst), 1);
    assert_eq!(
        first_slack.posted_messages().len() + second_slack.posted_messages().len(),
        1
    );
}

#[tokio::test]
async fn finance_notice_recovers_acceptance_before_step_checkpoint_without_duplicate() {
    let Some(db) = database().await else { return };
    let runner = Runner(Arc::new(AtomicUsize::new(0)));
    let slack = Arc::new(AcceptThenLoseReply::default());
    let dbos = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-lost-reply",
    ));
    let workflow =
        dbos_reconcile::register(&dbos, context(runner.clone(), slack.clone())).expect("register");
    dbos.launch().await.expect("launch");
    dbos_reconcile::fire(&workflow, slot())
        .await
        .expect("start");
    let report = result(&dbos, &dbos_reconcile::workflow_id(slot())).await;
    dbos.shutdown().await;

    assert_eq!(report.ingested, 2);
    assert_eq!(
        runner.0.load(Ordering::SeqCst),
        1,
        "reconcile step is checkpointed"
    );
    assert_eq!(
        slack.attempts.load(Ordering::SeqCst),
        1,
        "receipt lookup prevents repost"
    );
    assert_eq!(slack.inner.posted_messages().len(), 1);
}

#[tokio::test]
async fn executor_restart_reuses_reconcile_checkpoint_and_finance_receipt() {
    let Some(db) = database().await else { return };
    let runner = Runner(Arc::new(AtomicUsize::new(0)));
    let slack = Arc::new(AcceptThenHold::default());
    let id = dbos_reconcile::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-restart",
    ));
    let workflow = dbos_reconcile::register(&first, context(runner.clone(), slack.clone()))
        .expect("register first");
    first.launch().await.expect("launch first");
    dbos_reconcile::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, slack.accepted.notified())
        .await
        .expect("finance notice accepted");
    first.shutdown().await;

    let checkpoints = sqlx::query_scalar::<_, String>(
        "SELECT function_name FROM dbos.operation_outputs WHERE workflow_uuid = $1 ORDER BY function_id",
    )
    .bind(&id)
    .fetch_all(&db.pool)
    .await
    .expect("read workflow checkpoints");
    assert_eq!(checkpoints, ["reconcile"]);

    let recovered = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-restart",
    ));
    dbos_reconcile::register(&recovered, context(runner.clone(), slack.clone()))
        .expect("register recovery executor");
    recovered.launch().await.expect("relaunch executor");
    let report = result(&recovered, &id).await;
    recovered.shutdown().await;

    assert_eq!(report.ingested, 2);
    assert_eq!(
        runner.0.load(Ordering::SeqCst),
        1,
        "reconcile checkpoint reused"
    );
    assert_eq!(
        slack.attempts.load(Ordering::SeqCst),
        1,
        "receipt suppresses retry"
    );
    assert_eq!(slack.inner.posted_messages().len(), 1);
}

#[tokio::test]
async fn executor_restart_does_not_repeat_provider_mirror_writes() {
    let Some(db) = database().await else { return };
    let surreal = store::surreal::test_support::mem().await;
    let entity = store::test_support::seed_entity(&surreal).await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "dbos-reconcile".into(),
            name: "DBOS reconcile synthetic project".into(),
            status: "open".into(),
            entity_id: entity,
            ..Default::default()
        },
    )
    .await
    .expect("seed synthetic Project");
    let provider = Arc::new(StubBillingProvider::new());
    provider.set_listed_invoices(vec![ReceivableInvoice {
        invoice_id: "synthetic-xero-invoice-1".into(),
        reference: "Matter dbos-reconcile".into(),
        status: "AUTHORISED".into(),
        amount_cents: 12500,
        amount_paid_cents: 0,
        currency: "USD".into(),
        issued_at: slot(),
        due_at: None,
    }]);
    let runner = Arc::new(ActualReconcileRunner {
        provider: provider.clone(),
        surreal: surreal.clone(),
    });
    let slack = Arc::new(AcceptThenHold::default());
    let context = || {
        Context::with_runner(
            runner.clone(),
            slack.clone(),
            "synthetic-finance".into(),
            true,
        )
    };
    let id = dbos_reconcile::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-provider-restart",
    ));
    let workflow = dbos_reconcile::register(&first, context()).expect("register first");
    first.launch().await.expect("launch first");
    dbos_reconcile::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, slack.accepted.notified())
        .await
        .expect("finance notice accepted after mirror writes");
    first.shutdown().await;
    let calls_after_first_attempt = provider.calls();

    let recovered = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-provider-restart",
    ));
    dbos_reconcile::register(&recovered, context()).expect("register recovery");
    recovered.launch().await.expect("relaunch executor");
    let report = result(&recovered, &id).await;
    recovered.shutdown().await;

    let invoices = store::xero_invoices::for_projects(&surreal, &[project.id])
        .await
        .expect("read mirrored invoices");
    assert_eq!(report.ingested, 1);
    assert_eq!(
        provider.calls(),
        calls_after_first_attempt,
        "provider reconciliation step was checkpointed"
    );
    assert_eq!(
        invoices.len(),
        1,
        "natural provider invoice id has one mirror row"
    );
    assert_eq!(invoices[0].xero_invoice_id, "synthetic-xero-invoice-1");
    assert_eq!(slack.inner.posted_messages().len(), 1);
}

#[tokio::test]
async fn mid_step_restart_reuses_natural_trust_transaction_identity() {
    let Some(db) = database().await else { return };
    let surreal = store::surreal::test_support::mem().await;
    let project_id = seed_trust_deposit(&surreal).await;

    let provider = Arc::new(StubBillingProvider::new());
    provider.set_listed_bank_accounts(vec![TrustBankAccount {
        account_id: "synthetic-iolta-nv".into(),
        code: Some("090".into()),
        name: "IOLTA NV — Trust".into(),
        currency: "USD".into(),
        balance_cents: 50000,
    }]);
    provider.set_listed_trust_transactions(vec![TrustBankTransaction {
        transaction_id: "synthetic-xero-bank-transaction-7".into(),
        kind: TrustTransactionKind::Receive,
        account_id: "synthetic-iolta-nv".into(),
        reference: format!("Matter {project_id}"),
        amount_cents: 12500,
        currency: "USD".into(),
        occurred_at: slot(),
        line_items: Vec::new(),
    }]);
    let runner = Arc::new(HoldAfterMirror {
        actual: ActualReconcileRunner {
            provider: provider.clone(),
            surreal: surreal.clone(),
        },
        calls: AtomicUsize::new(0),
        wrote: Notify::new(),
    });
    let slack = Arc::new(CapturingSlackBot::default());
    let context = || {
        Context::with_runner(
            runner.clone(),
            slack.clone(),
            "synthetic-finance".into(),
            true,
        )
    };
    let id = dbos_reconcile::workflow_id(slot());
    let first = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-mid-step-restart",
    ));
    let workflow = dbos_reconcile::register(&first, context()).expect("register first");
    first.launch().await.expect("launch first");
    dbos_reconcile::fire(&workflow, slot())
        .await
        .expect("start");
    tokio::time::timeout(DEADLINE, runner.wrote.notified())
        .await
        .expect("mirror writes completed before the step was interrupted");
    first.shutdown().await;
    let position = store::trust::position_for_project(&surreal, project_id)
        .await
        .expect("read trust position before recovery");
    assert_eq!(position.deposited_cents, 12500);

    let recovered = dbos::DBOS::new(dbos_engine::config_with_executor_id(
        &db.url,
        APP_VERSION,
        "reconcile-mid-step-restart",
    ));
    dbos_reconcile::register(&recovered, context()).expect("register recovery");
    recovered.launch().await.expect("relaunch executor");
    let _report = result(&recovered, &id).await;
    recovered.shutdown().await;

    let movements = store::trust::movements_for_project(&surreal, project_id)
        .await
        .expect("read trust ledger");
    assert_eq!(
        movements
            .iter()
            .filter(|movement| movement.external_ref.as_deref()
                == Some("synthetic-xero-bank-transaction-7"))
            .count(),
        1,
        "natural provider transaction identity deduplicates the replayed write"
    );
    assert_eq!(
        store::trust::position_for_project(&surreal, project_id)
            .await
            .expect("read final trust position")
            .deposited_cents,
        12500
    );
    assert_eq!(
        runner.calls.load(Ordering::SeqCst),
        2,
        "uncheckpointed step replays"
    );
    assert_eq!(slack.posted_messages().len(), 1);
}
