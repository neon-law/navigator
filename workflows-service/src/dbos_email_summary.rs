//! DBOS adapter for one archived support-email summary.
//!
//! The webhook submits only an opaque receipt ID and immutable run
//! configuration, and the workflow id is derived from that receipt, so a
//! redelivered message joins the same durable run instead of summarizing
//! twice. Every archive read, provider request, and Slack delivery is a
//! named DBOS step mirroring the Restate `EmailSummary::run` journal, so a
//! recovery replays step results instead of repeating side effects.

use std::sync::Arc;

use cloud::StorageService;
use dbos::{WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use workflows::{EmailSummaryRequest, ProviderDelivery, SlackBot, SummaryProvider};

use crate::email_summary::{
    check_sender_step, summarize_provider_authorized, DeliveryTarget, EmailSummaryReport,
    SenderGate, SummaryProviders,
};

pub const WORKFLOW: &str = "EmailSummary";
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED";

pub type EmailSummaryWorkflow =
    WorkflowRef<EmailSummaryRequest, EmailSummaryReport, EmailSummaryError>;

/// Whether the `EmailSummary` twin is enabled in the DBOS host. Disabled
/// unless explicitly set, so enabling the executor alone cannot duplicate the
/// live Restate lane.
pub fn enabled(get: impl Fn(&str) -> Option<String>) -> Result<bool, &'static str> {
    match get(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err("NAVIGATOR_DBOS_EMAIL_SUMMARY_ENABLED must be true or false"),
    }
}

/// The workflow id every delivery of one email receipt joins. The Restate
/// invocation key is the receipt id; preserving it as the DBOS id keeps
/// duplicate deliveries deduplicated on either engine.
#[must_use]
pub fn workflow_id(receipt_id: uuid::Uuid) -> String {
    workflows::trigger::email_summary_workflow_id(receipt_id)
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum EmailSummaryError {
    #[error("the email receipt could not be loaded")]
    Receipt,
    #[error("summary run configuration does not match receipt")]
    Config,
    #[error("the sender gate step failed: {0}")]
    SenderGate(String),
    #[error("the summary provider step failed: {0}")]
    Provider(String),
    #[error("the summary delivery failed: {0}")]
    Delivery(String),
}

#[derive(Clone)]
pub struct Context {
    surreal: store::surreal::SurrealDb,
    storage: Arc<dyn StorageService>,
    slack: Arc<dyn SlackBot>,
    providers: Option<SummaryProviders>,
}

impl Context {
    #[must_use]
    pub fn new(
        surreal: store::surreal::SurrealDb,
        storage: Arc<dyn StorageService>,
        slack: Arc<dyn SlackBot>,
        providers: Option<SummaryProviders>,
    ) -> Self {
        Self {
            surreal,
            storage,
            slack,
            providers,
        }
    }
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<EmailSummaryWorkflow> {
    dbos.register_workflow(WORKFLOW, move |request: EmailSummaryRequest| {
        let context = context.clone();
        async move { run_summary(context, request).await }
    })
}

/// The journaled sequence behind the Restate `run` handler. Step names match
/// the Restate journal so a run reads the same on either engine.
async fn run_summary(
    context: Context,
    request: EmailSummaryRequest,
) -> dbos::Result<EmailSummaryReport, EmailSummaryError> {
    let receipt_id = request.receipt_id;
    let receipt = {
        let surreal = context.surreal.clone();
        dbos::step("load-receipt", move || {
            let surreal = surreal.clone();
            async move {
                match store::email_receipts::find_by_id(&surreal, receipt_id).await {
                    Ok(Some(receipt)) => Ok(receipt),
                    Ok(None) | Err(_) => Err(dbos::Error::Application(EmailSummaryError::Receipt)),
                }
            }
        })
        .await?
    };

    if request.gemini.input_digest != receipt.raw_digest {
        return Err(dbos::Error::Application(EmailSummaryError::Config));
    }

    let gate = {
        let storage = Arc::clone(&context.storage);
        let surreal = context.surreal.clone();
        let receipt = receipt.clone();
        let matter_project_id = request.matter_project_id;
        dbos::step("check-sender", move || {
            let (storage, surreal, receipt) =
                (Arc::clone(&storage), surreal.clone(), receipt.clone());
            async move {
                check_sender_step(storage, &surreal, &receipt, matter_project_id)
                    .await
                    .map_err(|error| {
                        // Restate's `HandlerError` has no `Display`; journal
                        // its `Debug` so a recovery can see what failed.
                        dbos::Error::Application(EmailSummaryError::SenderGate(format!(
                            "{error:?}"
                        )))
                    })
            }
        })
        .await?
    };

    let gemini = match gate {
        SenderGate::Unknown { reason } => ProviderDelivery::Skipped { reason },
        SenderGate::Known => {
            summarize_step(&context, receipt_id, receipt.clone(), &request).await?
        }
    };

    let delivery_state = {
        let target = DeliveryTarget {
            surreal: context.surreal.clone(),
            slack: Arc::clone(&context.slack),
            storage: Arc::clone(&context.storage),
            receipt,
            config: request.gemini.clone(),
            channel_id: request.channel_id.clone(),
        };
        dbos::step("deliver-slack", move || {
            let target = target.clone();
            let gemini = gemini.clone();
            async move {
                target.deliver(gemini).await.map_err(|error| {
                    dbos::Error::Application(EmailSummaryError::Delivery(format!("{error:?}")))
                })
            }
        })
        .await?
    };

    Ok(EmailSummaryReport {
        receipt_id,
        delivery_state,
    })
}

/// The journaled provider step, run only for known senders. Mirrors the
/// Restate `EmailSummary` journal entry-for-entry.
async fn summarize_step(
    context: &Context,
    receipt_id: uuid::Uuid,
    receipt: store::email_receipts::EmailReceipt,
    request: &EmailSummaryRequest,
) -> dbos::Result<ProviderDelivery, EmailSummaryError> {
    let storage = Arc::clone(&context.storage);
    let providers = context.providers.clone();
    let surreal = context.surreal.clone();
    let config = request.gemini.clone();
    let project_id = request.project_id.clone();
    let matter_project_id = request.matter_project_id;
    dbos::step("summarize-gemini", move || {
        let (storage, surreal, receipt, config) = (
            Arc::clone(&storage),
            surreal.clone(),
            receipt.clone(),
            config.clone(),
        );
        let providers = providers.clone();
        let project_id = project_id.clone();
        async move {
            let started = std::time::Instant::now();
            let result = summarize_provider_authorized(
                storage,
                surreal,
                providers,
                SummaryProvider::Gemini,
                receipt.clone(),
                config,
                project_id,
                matter_project_id,
            )
            .await;
            telemetry::record_email_stage(
                &receipt_id.to_string(),
                telemetry::EmailStage::Summary,
                match &result {
                    Ok(ProviderDelivery::Succeeded { .. }) => telemetry::EmailOutcome::Succeeded,
                    Ok(ProviderDelivery::Skipped { .. }) => telemetry::EmailOutcome::Skipped,
                    _ => telemetry::EmailOutcome::Failed,
                },
                started.elapsed(),
            );
            result.map_err(|error| {
                // Restate's `HandlerError` has no `Display`; journal its
                // `Debug` so a recovery can see what failed.
                dbos::Error::Application(EmailSummaryError::Provider(format!("{error:?}")))
            })
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::postgres::{PgConnectOptions, PgSslMode};
    use store::persons::{set_email_confirmed, NewPerson, Role};
    use uuid::Uuid;
    use workflows::{
        CapturingSlackBot, EmailSummaryRequest, EmailSummaryRunConfig, SlackBot, SummaryProvider,
    };

    use super::{
        enabled, register, workflow_id, Context, EmailSummaryError, EmailSummaryWorkflow,
        ENABLED_ENV, WORKFLOW,
    };
    use crate::email_summary::EmailSummaryReport;

    const MAILBOX: &str = "support@example.com";
    const DEPLOYMENT: &str = "staging";
    const ACCOUNT: &str = "sendgrid-example";
    const ARCHIVE_KEY: &str = "inbound/dbos-summary.eml";
    const CHANNEL: &str = "C-SUMMARY";
    const ARCHIVE: &[u8] = b"From: sender@example.com\r\n\
        To: intake@example.com\r\n\
        Content-Type: text/plain\r\n\r\n\
        A bounded summary body\r\n";

    #[test]
    fn every_failure_names_its_step_without_message_content() {
        assert_eq!(
            EmailSummaryError::Receipt.to_string(),
            "the email receipt could not be loaded"
        );
        assert_eq!(
            EmailSummaryError::Config.to_string(),
            "summary run configuration does not match receipt"
        );
        assert_eq!(
            EmailSummaryError::SenderGate("store".into()).to_string(),
            "the sender gate step failed: store"
        );
        assert_eq!(
            EmailSummaryError::Provider("vertex".into()).to_string(),
            "the summary provider step failed: vertex"
        );
        assert_eq!(
            EmailSummaryError::Delivery("slack".into()).to_string(),
            "the summary delivery failed: slack"
        );
        let journaled = serde_json::to_string(&EmailSummaryError::Provider("x".into())).unwrap();
        let replayed: EmailSummaryError = serde_json::from_str(&journaled).unwrap();
        assert!(matches!(replayed, EmailSummaryError::Provider(reason) if reason == "x"));
    }

    /// A fresh synthetic database on the test PostgreSQL server, or `None`
    /// when none is configured. Mirrors the `tests/dbos` support lane.
    /// Returns a cleanup guard alongside the URL: dropping it removes the
    /// database even when the test fails mid-body, so long-lived dev
    /// servers never accumulate `dbos_email_summary_*` databases.
    async fn database_url() -> Option<(String, TestDatabaseCleanup)> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
                "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
            );
            return None;
        };
        let port: u16 = port
            .parse()
            .expect("NAVIGATOR_TEST_POSTGRES_PORT is a port");
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("synthetic")
            .database(
                &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                    .unwrap_or_else(|_| "smoke".into()),
            )
            .ssl_mode(PgSslMode::Disable);
        let admin = sqlx::PgPool::connect_with(options)
            .await
            .expect("connect to the test server");
        let name = format!("dbos_email_summary_{}", Uuid::now_v7().simple());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .execute(&admin)
            .await
            .expect("create the test database");
        admin.close().await;
        Some((
            format!("postgres://postgres:synthetic@127.0.0.1:{port}/{name}?sslmode=disable"),
            TestDatabaseCleanup { port, name },
        ))
    }

    /// Owns one isolated test database and drops it when it leaves scope —
    /// on success, an early return, or a panic unwind alike. The work runs
    /// on a separate thread with its own runtime because `Drop` fires
    /// inside the test's tokio runtime, where a nested `block_on` would
    /// panic. Best-effort: a failure is printed, never raised, from Drop.
    struct TestDatabaseCleanup {
        port: u16,
        name: String,
    }

    impl Drop for TestDatabaseCleanup {
        fn drop(&mut self) {
            let port = self.port;
            let name = self.name.clone();
            let result = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build the database cleanup runtime")
                    .block_on(async {
                        let options = PgConnectOptions::new()
                            .host("127.0.0.1")
                            .port(port)
                            .username("postgres")
                            .password("synthetic")
                            .ssl_mode(PgSslMode::Disable);
                        let admin = match sqlx::PgPool::connect_with(options).await {
                            Ok(admin) => admin,
                            Err(err) => {
                                eprintln!("test database {name}: reconnect for drop failed: {err}");
                                return;
                            }
                        };
                        let terminate = sqlx::query(
                            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1",
                        )
                        .bind(&name)
                        .execute(&admin)
                        .await;
                        let drop =
                            sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {name}")))
                                .execute(&admin)
                                .await;
                        let outcome = terminate.and(drop);
                        admin.close().await;
                        if let Err(err) = outcome {
                            eprintln!("test database {name}: drop failed: {err}");
                        }
                    });
            })
            .join();
            if result.is_err() {
                eprintln!("test database {}: cleanup thread panicked", self.name);
            }
        }
    }

    /// A Firm whose Client DRI is an approved, confirmed sender on one
    /// Project, as the summary sender gate requires.
    async fn approved_sender(db: &store::surreal::SurrealDb) -> (Uuid, Uuid) {
        let admin = store::persons::create(
            db,
            &NewPerson::with_role(
                "Summary Admin",
                format!("summary-admin-{}@example.com", Uuid::now_v7()),
                Role::Admin,
            ),
        )
        .await
        .unwrap();
        let firm = store::firms::create(
            db,
            &store::firms::NewFirm {
                name: format!("Summary Firm {}", admin.id),
                status: "active".into(),
                entity_id: store::test_support::seed_entity(db).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        let sender =
            store::persons::create(db, &NewPerson::new("Sam Sender", "sender@example.com"))
                .await
                .unwrap();
        set_email_confirmed(db, sender.id, true).await.unwrap();
        let project = store::projects::create(
            db,
            &store::projects::NewProject {
                code: format!("summary-{}", Uuid::now_v7()),
                name: "Summary matter".into(),
                status: "open".into(),
                entity_id: store::test_support::seed_entity(db).await,
                firm_id: Some(firm.id),
                ..store::projects::NewProject::default()
            },
        )
        .await
        .unwrap();
        store::projects::designate_dri_in_surreal(
            db,
            project.id,
            sender.id,
            store::projects::DriSide::Client,
        )
        .await
        .unwrap();
        store::summary_sender_approvals::approve(
            db,
            Role::Admin,
            admin.id,
            firm.id,
            project.id,
            ACCOUNT,
            "sender@example.com",
        )
        .await
        .unwrap();
        (firm.id, project.id)
    }

    async fn seed_receipt(db: &store::surreal::SurrealDb, firm_id: Uuid) -> (Uuid, String) {
        let digest = workflows::scoped_email_digest(DEPLOYMENT, MAILBOX, ARCHIVE);
        let ensured = store::email_receipts::ensure(
            db,
            &store::email_receipts::NewEmailReceipt {
                receiving_mailbox: MAILBOX,
                deployment: DEPLOYMENT,
                firm_id: Some(firm_id),
                provider_account: Some(ACCOUNT),
                sender_authenticated: true,
                raw_digest: &digest,
                source_message_id: None,
                archive_key: ARCHIVE_KEY,
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .expect("seed the email receipt");
        (ensured.receipt.id, digest)
    }

    fn request(
        receipt_id: Uuid,
        matter_project_id: Option<Uuid>,
        digest: &str,
    ) -> EmailSummaryRequest {
        EmailSummaryRequest {
            receipt_id,
            matter_project_id,
            project_id: "synthetic-project".into(),
            channel_id: CHANNEL.into(),
            gemini: EmailSummaryRunConfig::new(
                SummaryProvider::Gemini,
                "summary-model",
                "global",
                "summary-v1",
                digest,
            )
            .expect("valid test config"),
        }
    }

    async fn run(
        workflow: &EmailSummaryWorkflow,
        request: EmailSummaryRequest,
    ) -> dbos::Result<EmailSummaryReport, EmailSummaryError> {
        let id = workflow_id(request.receipt_id);
        tokio::time::timeout(
            std::time::Duration::from_mins(1),
            workflow.run_with(
                request,
                dbos::RunOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            ),
        )
        .await
        .expect("EmailSummary did not finish")
    }

    #[tokio::test]
    async fn the_dbos_twin_gates_summarizes_and_delivers_once_per_receipt() {
        // `_cleanup` (not `_`) keeps the guard alive to the end of the test.
        let Some((url, _cleanup)) = database_url().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let (firm_id, project_id) = approved_sender(&surreal).await;
        let (receipt_id, digest) = seed_receipt(&surreal, firm_id).await;
        let dir = tempfile::tempdir().expect("temp archive dir");
        let storage = cloud::FsStorage::new(dir.path())
            .await
            .expect("temp FsStorage");
        cloud::StorageService::put(&storage, ARCHIVE_KEY, ARCHIVE, "message/rfc822")
            .await
            .expect("archive the message");
        let slack = CapturingSlackBot::new();
        let bot: Arc<dyn SlackBot> = Arc::new(slack.clone());

        let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&url, "test"));
        let workflow = register(
            &dbos,
            Context::new(surreal.clone(), Arc::new(storage), bot, None),
        )
        .expect("register EmailSummary");
        dbos.launch().await.expect("launch DBOS");

        let missing = run(&workflow, request(Uuid::now_v7(), None, &digest)).await;
        assert!(
            matches!(
                missing,
                Err(dbos::Error::Application(EmailSummaryError::Receipt))
            ),
            "an unknown receipt fails at load-receipt: {missing:?}"
        );

        let other_receipt = store::email_receipts::ensure(
            &surreal,
            &store::email_receipts::NewEmailReceipt {
                receiving_mailbox: MAILBOX,
                deployment: DEPLOYMENT,
                firm_id: Some(firm_id),
                provider_account: Some(ACCOUNT),
                sender_authenticated: true,
                raw_digest: &"b".repeat(64),
                source_message_id: None,
                archive_key: ARCHIVE_KEY,
                letter_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap()
        .receipt
        .id;
        let mismatched = run(&workflow, request(other_receipt, None, &digest)).await;
        assert!(
            matches!(
                mismatched,
                Err(dbos::Error::Application(EmailSummaryError::Config))
            ),
            "a run configuration for a different archive is refused: {mismatched:?}"
        );

        let report = run(&workflow, request(receipt_id, Some(project_id), &digest))
            .await
            .expect("a known sender's summary is delivered");
        assert_eq!(report.receipt_id, receipt_id);
        assert_eq!(report.delivery_state, "confirmed");

        let replayed = run(&workflow, request(receipt_id, Some(project_id), &digest))
            .await
            .expect("a redelivery joins the finished run");
        assert_eq!(replayed, report);
        dbos.shutdown().await;

        let posted = slack.posted_messages();
        assert_eq!(posted.len(), 1, "one Slack post per receipt: {posted:?}");
        assert_eq!(posted[0].0, CHANNEL);
        assert!(
            posted[0].1.contains("provider_not_configured"),
            "the unconfigured provider is reported, not hidden: {}",
            posted[0].1
        );
    }

    #[tokio::test]
    async fn an_unassigned_message_skips_the_provider_and_still_posts() {
        let Some((url, _cleanup)) = database_url().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let firm_id = store::test_support::seed_firm_with_brand(&surreal, "neon").await;
        let (receipt_id, digest) = seed_receipt(&surreal, firm_id).await;
        let dir = tempfile::tempdir().expect("temp archive dir");
        let storage = cloud::FsStorage::new(dir.path())
            .await
            .expect("temp FsStorage");
        cloud::StorageService::put(&storage, ARCHIVE_KEY, ARCHIVE, "message/rfc822")
            .await
            .expect("archive the message");
        let slack = CapturingSlackBot::new();
        let bot: Arc<dyn SlackBot> = Arc::new(slack.clone());

        let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&url, "test"));
        let workflow = register(
            &dbos,
            Context::new(surreal.clone(), Arc::new(storage), bot, None),
        )
        .expect("register EmailSummary");
        dbos.launch().await.expect("launch DBOS");
        let report = run(&workflow, request(receipt_id, None, &digest))
            .await
            .expect("an unassigned message is delivered without a summary");
        dbos.shutdown().await;

        assert_eq!(report.delivery_state, "confirmed");
        let posted = slack.posted_messages();
        assert_eq!(posted.len(), 1);
        assert!(
            posted[0]
                .1
                .contains("skipped\nReason: message is not assigned to a Project"),
            "the provider step never ran: {}",
            posted[0].1
        );
    }

    #[test]
    fn registration_is_opt_in_and_rejects_invalid_values() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|key| (key == ENABLED_ENV).then(|| "false".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "true".to_owned())).unwrap());
        assert!(enabled(|key| (key == ENABLED_ENV).then(|| "yes".to_owned())).is_err());
    }

    #[test]
    fn every_delivery_of_one_receipt_joins_one_workflow_id() {
        let receipt_id = uuid::Uuid::now_v7();
        assert_eq!(
            workflow_id(receipt_id),
            format!("EmailSummary:{receipt_id}")
        );
        assert_eq!(workflow_id(receipt_id), workflow_id(receipt_id));
        assert_ne!(workflow_id(uuid::Uuid::now_v7()), workflow_id(receipt_id));
        assert_eq!(WORKFLOW, "EmailSummary");
    }
}
