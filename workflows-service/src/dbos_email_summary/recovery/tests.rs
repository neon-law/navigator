//! Real DBOS recovery over the production summary body and native PostgreSQL admission.

use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use cloud::StorageService as _;
use std::{future::Future, time::Duration};
use tower::ServiceExt as _;
use uuid::Uuid;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

const VERSION: &str = "summary-recovery-test";
const TOKEN: &str = "synthetic-summary-worker";
const ARCHIVE: &[u8] = b"From: sender@example.test\r\nTo: intake@example.test\r\nContent-Type: text/plain\r\n\r\nA synthetic request.\r\n";

struct Case {
    pg: store::postgres::PostgresDb,
    dbos: dbos::DBOS,
    url: String,
    summary: super::super::EmailSummaryWorkflow,
    router: axum::Router,
    request: workflows::EmailSummaryRequest,
    slack: Arc<workflows::CapturingSlackBot>,
    slack_http: Option<MockServer>,
    provider: MockServer,
    files: tempfile::TempDir,
}

async fn summary_entity(source: &store::application_store::ApplicationStore) -> Uuid {
    let id = Uuid::new_v4();
    let jurisdiction = store::jurisdictions::create(
        source,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {id}"),
            id.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(source, &format!("Synthetic kind {id}"))
        .await
        .unwrap();
    store::entities::create(
        source,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {id}"),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn approved_project(source: &store::application_store::ApplicationStore) -> (Uuid, Uuid) {
    let admin = store::persons::create(
        source,
        &store::persons::NewPerson::with_role(
            "Synthetic Admin",
            "admin@example.test",
            store::persons::Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        source,
        &store::firms::NewFirm {
            name: "Synthetic Practice".into(),
            status: "active".into(),
            entity_id: summary_entity(source).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    store::brands::create(
        source,
        store::persons::Role::Admin,
        Some(admin.id),
        &store::brands::NewBrand {
            name: "Synthetic Brand".into(),
            key: "summary-test".into(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(
        source,
        store::persons::Role::Owner,
        None,
        firm.id,
        "summary-test",
    )
    .await
    .unwrap();
    let project = store::projects::create(
        source,
        &store::projects::NewProject {
            code: "synthetic-summary".into(),
            name: "Synthetic Summary".into(),
            status: "open".into(),
            brand: "summary-test".into(),
            entity_id: summary_entity(source).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let sender = store::persons::create(
        source,
        &store::persons::NewPerson::with_role(
            "Synthetic Sender",
            "sender@example.test",
            store::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    store::persons::set_email_confirmed(source, sender.id, true)
        .await
        .unwrap();
    store::projects::designate_dri(
        source,
        project.id,
        sender.id,
        store::projects::DriSide::Client,
    )
    .await
    .unwrap();
    store::summary_sender_approvals::approve(
        source,
        store::persons::Role::Admin,
        admin.id,
        firm.id,
        project.id,
        "synthetic-inbound",
        "sender@example.test",
    )
    .await
    .unwrap();
    (firm.id, project.id)
}

async fn archived_request(
    source: &store::application_store::ApplicationStore,
    firm: Uuid,
    project: Uuid,
) -> workflows::EmailSummaryRequest {
    let digest = workflows::scoped_email_digest("synthetic", "intake@example.test", ARCHIVE);
    let receipt = store::email_receipts::ensure(
        source,
        &store::email_receipts::NewEmailReceipt {
            receiving_mailbox: "intake@example.test",
            deployment: "synthetic",
            firm_id: Some(firm),
            provider_account: Some("synthetic-inbound"),
            sender_authenticated: true,
            raw_digest: &digest,
            source_message_id: None,
            archive_key: "inbound/synthetic.eml",
            letter_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap()
    .receipt;
    workflows::EmailSummaryRequest {
        receipt_id: receipt.id,
        matter_project_id: Some(project),
        project_id: "synthetic-project".into(),
        channel_id: "C-SYNTHETIC".into(),
        gemini: workflows::EmailSummaryRunConfig::new(
            workflows::SummaryProvider::Gemini,
            "synthetic-model",
            "global",
            "summary-v1",
            digest,
        )
        .unwrap(),
    }
}

impl Case {
    async fn new(pg: store::postgres::PostgresDb, http_slack: bool) -> Self {
        let source = store::application_store::ApplicationStore::uninitialized();
        let (firm, project) = approved_project(&source).await;
        let request = archived_request(&source, firm, project).await;
        let files = tempfile::tempdir().unwrap();
        let storage = Arc::new(cloud::FsStorage::new(files.path()).await.unwrap());
        storage
            .put("inbound/synthetic.eml", ARCHIVE, "message/rfc822")
            .await
            .unwrap();
        let provider = MockServer::start().await;
        let providers = crate::email_summary::SummaryProviders {
            gemini: cloud::GeminiVertexAdapter::new(Arc::new(cloud::StaticTokenSource::new(
                "synthetic-token",
            )))
            .unwrap()
            .with_base_url(provider.uri()),
        };
        let slack = Arc::new(workflows::CapturingSlackBot::new());
        let slack_http = if http_slack {
            Some(MockServer::start().await)
        } else {
            None
        };
        let runtime_slack: Arc<dyn workflows::SlackBot> = if let Some(server) = &slack_http {
            Arc::new(workflows::SlackBotClient::with_base_url(
                "synthetic-slack-token",
                server.uri(),
            ))
        } else {
            slack.clone()
        };
        let context = super::super::Context::new(source, storage, runtime_slack, Some(providers));
        let options = pg.pool().connect_options();
        let url = format!(
            "postgresql://postgres:synthetic@127.0.0.1:{}/{}?sslmode=disable",
            options.get_port(),
            options.get_database().unwrap()
        );
        let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&url, VERSION));
        let callback_pg = pg.clone();
        // Scope only the connection used by the actual production body. Its
        // ordinary accessors still require the fixture's real enrollment history.
        let summary = dbos
            .register_workflow(
                super::super::WORKFLOW,
                move |request: workflows::EmailSummaryRequest| {
                    let context = context.clone();
                    store::postgres::PostgresDb::with_test_db(
                        callback_pg.clone(),
                        super::super::run_summary(context, request),
                    )
                },
            )
            .unwrap();
        let recovery = register(&dbos, pg.clone(), VERSION.into()).unwrap();
        dbos.launch().await.unwrap();
        let router = crate::health::router_with_dbos_ingress(
            crate::dbos_ingress::WorkerIngress {
                email_summary_recovery: Some(RecoveryIngress::new(
                    TOKEN.into(),
                    dbos.clone(),
                    pg.clone(),
                    recovery,
                    VERSION.into(),
                )),
                ..Default::default()
            },
            None,
        );
        Self {
            pg,
            dbos,
            url,
            summary,
            router,
            request,
            slack,
            slack_http,
            provider,
            files,
        }
    }

    async fn failed_summary(&self) {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&self.provider)
            .await;
        let report = self
            .summary
            .run_with(
                self.request.clone(),
                dbos::RunOptions {
                    workflow_id: Some(&super::super::workflow_id(self.request.receipt_id)),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.delivery_state, store::email_deliveries::CONFIRMED);
        assert_eq!(self.slack.posted_messages().len(), 1);
        let steps = self
            .dbos
            .list_workflow_steps(&super::super::workflow_id(self.request.receipt_id))
            .await
            .unwrap();
        let result: ProviderDelivery = serde_json::from_str(
            steps
                .iter()
                .find(|step| step.step_name == "summarize-gemini")
                .unwrap()
                .output
                .as_deref()
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(result, ProviderDelivery::Failed { .. }));
        self.provider.reset().await;
        self.allow_success().await;
    }

    async fn allow_success(&self) {
        Mock::given(method("POST"))
            .respond_with(provider_success())
            .mount(&self.provider)
            .await;
    }

    fn command(&self) -> EmailSummaryRecoveryRequest {
        EmailSummaryRecoveryRequest {
            operation_id: Uuid::now_v7(),
            from_recovery: None,
            after_failed_result: Some("synthetic-operator".into()),
            request: self.request.clone(),
        }
    }

    async fn submit(&self, command: &EmailSummaryRecoveryRequest, authorized: bool) -> StatusCode {
        let mut request = Request::post("/internal/dbos/email-summary/recover");
        if authorized {
            request = request.header("authorization", format!("Bearer {TOKEN}"));
        }
        self.router
            .clone()
            .oneshot(
                request
                    .body(Body::from(serde_json::to_vec(command).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn complete(&self, command: &EmailSummaryRecoveryRequest) -> RecoveryReport {
        let controller = self
            .dbos
            .retrieve_workflow::<RecoveryReport, RecoveryError>(&email_summary_recovery_id(
                command.operation_id,
            ))
            .unwrap();
        let report = tokio::time::timeout(Duration::from_secs(60), controller.result())
            .await
            .unwrap()
            .unwrap();
        let child = self
            .dbos
            .retrieve_workflow::<EmailSummaryReport, EmailSummaryError>(&report.target_workflow_id)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(60), child.result())
            .await
            .unwrap()
            .unwrap();
        report
    }
}

async fn with_case<F, Fut>(run: F)
where
    F: FnOnce(Arc<Case>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    with_case_mode(false, run).await;
}

async fn with_case_mode<F, Fut>(http_slack: bool, run: F)
where
    F: FnOnce(Arc<Case>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let case = Arc::new(Case::new(pg.clone(), http_slack).await);
        let result = tokio::spawn(store::postgres::PostgresDb::with_test_db(
            pg,
            run(Arc::clone(&case)),
        ))
        .await;
        case.dbos.shutdown().await;
        if let Err(error) = result {
            if error.is_panic() {
                std::panic::resume_unwind(error.into_panic());
            }
            panic!("summary recovery test task was canceled");
        }
    })
    .await;
}

#[tokio::test]
async fn provider_failure_recovery_keeps_receipt_checkpoints_and_repeated_operation_posts_once() {
    with_case(|case| async move {
        case.failed_summary().await;
        let command = case.command();
        assert_eq!(case.submit(&command, false).await, StatusCode::UNAUTHORIZED);
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        let report = case.complete(&command).await;
        assert_eq!(
            report.target_workflow_id,
            email_summary_recovery_child(case.request.receipt_id, command.operation_id)
        );
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        case.complete(&command).await;
        assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
        assert_eq!(case.slack.posted_messages().len(), 2);
        let source = case
            .dbos
            .list_workflow_steps(&super::super::workflow_id(case.request.receipt_id))
            .await
            .unwrap();
        let fork = case
            .dbos
            .list_workflow_steps(&report.target_workflow_id)
            .await
            .unwrap();
        for name in ["load-receipt", "check-sender"] {
            let original = source.iter().find(|step| step.step_name == name).unwrap();
            let replay = fork.iter().find(|step| step.step_name == name).unwrap();
            assert_eq!(original.step_id, replay.step_id);
            assert_eq!(original.output, replay.output);
        }
        let mut changed = command.clone();
        changed.request.receipt_id = Uuid::now_v7();
        assert_eq!(case.submit(&changed, true).await, StatusCode::CONFLICT);
        changed = command.clone();
        changed.after_failed_result = Some("another-operator".into());
        assert_eq!(case.submit(&changed, true).await, StatusCode::CONFLICT);
        assert_eq!(case.slack.posted_messages().len(), 2);
    })
    .await;
}

#[tokio::test]
async fn native_recovery_rejects_unsafe_delivery_wrong_input_and_concurrent_owners() {
    with_case(|case| async move {
        case.failed_summary().await;
        let mut command = case.command();
        command.after_failed_result = None;
        assert_eq!(case.submit(&command, true).await, StatusCode::CONFLICT);
        for state in ["sending", "unknown"] {
            sqlx::query("UPDATE email_delivery SET state=$2 WHERE receipt_id=$1")
                .bind(case.request.receipt_id)
                .bind(state)
                .execute(case.pg.pool())
                .await
                .unwrap();
            assert_eq!(
                case.submit(&case.command(), true).await,
                StatusCode::CONFLICT
            );
        }
        sqlx::query("UPDATE email_delivery SET state='confirmed' WHERE receipt_id=$1")
            .bind(case.request.receipt_id)
            .execute(case.pg.pool())
            .await
            .unwrap();
        let mut wrong = case.command();
        wrong.request.gemini.input_digest = "a".repeat(64);
        assert_eq!(case.submit(&wrong, true).await, StatusCode::CONFLICT);
        wrong = case.command();
        wrong.request.channel_id = "C-OTHER".into();
        assert_eq!(case.submit(&wrong, true).await, StatusCode::CONFLICT);
        let first = case.command();
        let second = case.command();
        let (a, b) = tokio::join!(case.submit(&first, true), case.submit(&second, true));
        assert!(matches!(a, StatusCode::ACCEPTED | StatusCode::CONFLICT));
        assert!(matches!(b, StatusCode::ACCEPTED | StatusCode::CONFLICT));
        let mut completed = 0;
        for (command, status) in [(&first, a), (&second, b)] {
            if status == StatusCode::ACCEPTED {
                let handle = case
                    .dbos
                    .retrieve_workflow::<RecoveryReport, RecoveryError>(&email_summary_recovery_id(
                        command.operation_id,
                    ))
                    .unwrap();
                if let Ok(report) = tokio::time::timeout(Duration::from_secs(60), handle.result())
                    .await
                    .unwrap()
                {
                    case.dbos
                        .retrieve_workflow::<EmailSummaryReport, EmailSummaryError>(
                            &report.target_workflow_id,
                        )
                        .unwrap()
                        .result()
                        .await
                        .unwrap();
                    completed += 1;
                }
            }
        }
        assert_eq!(completed, 1);
        assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
        assert_eq!(case.slack.posted_messages().len(), 2);
    })
    .await;
}

struct CheckpointGate {
    connection: sqlx::pool::PoolConnection<sqlx::Postgres>,
    table: &'static str,
    key: i64,
}

impl CheckpointGate {
    async fn install(case: &Case, operation: Uuid, after_fork: bool) -> Self {
        let (table, event, condition) = if after_fork {
            ("dbos.workflow_status", "UPDATE", "NEW.status = 'SUCCESS'")
        } else {
            (
                "dbos.operation_outputs",
                "INSERT",
                "NEW.function_name = 'claim-receipt-and-delivery'",
            )
        };
        Self::install_on(
            case,
            &email_summary_recovery_id(operation),
            table,
            event,
            condition,
        )
        .await
    }

    async fn install_on(
        case: &Case,
        id: &str,
        table: &'static str,
        event: &str,
        condition: &str,
    ) -> Self {
        let key = i64::from_be_bytes(Uuid::now_v7().as_bytes()[8..16].try_into().unwrap());
        let mut connection = case.pg.pool().acquire().await.unwrap();
        connection.close_on_drop();
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(key)
            .execute(&mut *connection)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE FUNCTION hold_summary_checkpoint() RETURNS trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF NEW.workflow_uuid = '{id}' AND {condition} THEN PERFORM pg_advisory_xact_lock({key}); END IF; RETURN NEW; END $$; \
             CREATE TRIGGER hold_summary_checkpoint BEFORE {event} ON {table} FOR EACH ROW EXECUTE FUNCTION hold_summary_checkpoint();"
        ))).execute(case.pg.pool()).await.unwrap();
        Self {
            connection,
            table,
            key,
        }
    }

    async fn unblock(mut self, case: &Case) {
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(self.key)
            .execute(&mut *self.connection)
            .await
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER hold_summary_checkpoint ON {}; DROP FUNCTION hold_summary_checkpoint();",
            self.table
        )))
        .execute(case.pg.pool())
        .await
        .unwrap();
        self.connection.close().await.unwrap();
    }

    async fn remove(mut self, case: &Case) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP TRIGGER hold_summary_checkpoint ON {}; DROP FUNCTION hold_summary_checkpoint();",
            self.table
        )))
        .execute(case.pg.pool())
        .await
        .unwrap();
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(self.key)
            .execute(&mut *self.connection)
            .await
            .unwrap();
        self.connection.close().await.unwrap();
    }
}

async fn wait_for(case: &Case, query: &'static str, operation: Uuid) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let reached: bool = sqlx::query_scalar(query)
                .bind(email_summary_recovery_id(operation))
                .fetch_one(case.pg.pool())
                .await
                .unwrap();
            if reached {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

async fn interrupted_checkpoint(case: &Case, after_fork: bool) {
    case.failed_summary().await;
    let command = case.command();
    let gate = CheckpointGate::install(case, command.operation_id, after_fork).await;
    assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
    let lock_query = if after_fork {
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query ILIKE '%workflow_status%' AND $1::text IS NOT NULL)"
    } else {
        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query ILIKE '%operation_outputs%' AND $1::text IS NOT NULL)"
    };
    wait_for(case, lock_query, command.operation_id).await;
    assert_eq!(
        ownership::current_operation(&case.pg, case.request.receipt_id)
            .await
            .unwrap(),
        Some(command.operation_id)
    );
    let checkpoint:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dbos.operation_outputs WHERE workflow_uuid=$1 AND function_name='claim-receipt-and-delivery')")
        .bind(email_summary_recovery_id(command.operation_id)).fetch_one(case.pg.pool()).await.unwrap();
    assert_eq!(checkpoint, after_fork);
    if after_fork {
        wait_for(case,"SELECT EXISTS(SELECT 1 FROM dbos.operation_outputs WHERE workflow_uuid=$1 AND function_name='DBOS.forkWorkflow')",command.operation_id).await;
        let child = email_summary_recovery_child(case.request.receipt_id, command.operation_id);
        tokio::time::timeout(
            Duration::from_secs(60),
            case.dbos
                .retrieve_workflow::<EmailSummaryReport, EmailSummaryError>(&child)
                .unwrap()
                .result(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(case.slack.posted_messages().len(), 2);
    } else {
        let state: String =
            sqlx::query_scalar("SELECT state FROM email_delivery WHERE receipt_id=$1")
                .bind(case.request.receipt_id)
                .fetch_one(case.pg.pool())
                .await
                .unwrap();
        assert_eq!(state, "not_attempted");
        assert!(case.provider.received_requests().await.unwrap().is_empty());
    }
    // Keep the lock held through shutdown. Releasing it before the worker is
    // stopped would permit the very checkpoint this test must prove was lost.
    case.dbos.shutdown().await;
    let checkpoint_after:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dbos.operation_outputs WHERE workflow_uuid=$1 AND function_name='claim-receipt-and-delivery')")
        .bind(email_summary_recovery_id(command.operation_id)).fetch_one(case.pg.pool()).await.unwrap();
    assert_eq!(checkpoint_after, after_fork);
    let status: String =
        sqlx::query_scalar("SELECT status FROM dbos.workflow_status WHERE workflow_uuid=$1")
            .bind(email_summary_recovery_id(command.operation_id))
            .fetch_one(case.pg.pool())
            .await
            .unwrap();
    assert_eq!(status, "PENDING");
    let blocked:i64=sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'")
        .fetch_one(case.pg.pool()).await.unwrap();
    assert_eq!(
        blocked, 0,
        "shutdown must cancel the blocked database write before releasing its gate"
    );
    gate.remove(case).await;
    case.dbos.launch().await.unwrap();
    case.complete(&command).await;
    assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
    case.complete(&command).await;
    assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
    assert_eq!(case.slack.posted_messages().len(), 2);
}

#[tokio::test]
async fn recovery_replays_atomic_reopen_when_its_admission_checkpoint_was_lost() {
    with_case(|case| async move {
        interrupted_checkpoint(&case, false).await;
    })
    .await;
}

#[tokio::test]
async fn committed_fork_survives_lost_controller_acknowledgement_without_repeating_effects() {
    with_case(|case| async move {
        interrupted_checkpoint(&case, true).await;
    })
    .await;
}

#[tokio::test]
async fn recorded_sender_error_recovers_its_named_stage_and_rejects_incompatible_version() {
    with_case(|case| async move {
        cloud::FsStorage::new(case.files.path())
            .await
            .unwrap()
            .delete("inbound/synthetic.eml")
            .await
            .unwrap();
        let source_id = super::super::workflow_id(case.request.receipt_id);
        assert!(matches!(
            case.summary
                .run_with(
                    case.request.clone(),
                    dbos::RunOptions {
                        workflow_id: Some(&source_id),
                        ..Default::default()
                    }
                )
                .await,
            Err(dbos::Error::Application(EmailSummaryError::SenderGate(_)))
        ));
        let source = read_record(&case.dbos, &source_id).await.unwrap().unwrap();
        assert_eq!(source.status, WorkflowStatus::Error);
        assert!(matches!(
            validate_source(&source, &case.request, "incompatible-version"),
            Err(RecoveryError::Version)
        ));
        let storage = cloud::FsStorage::new(case.files.path()).await.unwrap();
        storage
            .put("inbound/synthetic.eml", ARCHIVE, "message/rfc822")
            .await
            .unwrap();
        case.allow_success().await;
        let mut command = case.command();
        command.after_failed_result = None;
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        let report = case.complete(&command).await;
        let steps = case
            .dbos
            .list_workflow_steps(&report.target_workflow_id)
            .await
            .unwrap();
        assert!(steps
            .iter()
            .any(|step| step.step_name == "check-sender" && step.error.is_none()));
        assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
        assert_eq!(case.slack.posted_messages().len(), 1);
    })
    .await;
}

#[derive(Clone)]
struct RemoveArchiveAfterProvider {
    path: std::path::PathBuf,
}

impl wiremock::Respond for RemoveArchiveAfterProvider {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        std::fs::remove_file(&self.path).unwrap();
        provider_success()
    }
}

fn provider_success() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({"candidates":[{"content":{"parts":[{"text":"{\"summary\":\"Synthetic summary\",\"requested_actions\":[],\"sender_stated_dates\":[],\"missing_information\":[]}"}]},"finishReason":"STOP"}]}))
}

#[tokio::test]
async fn delivery_error_recovery_reuses_the_successful_provider_checkpoint() {
    with_case(|case| async move {
        Mock::given(method("POST"))
            .respond_with(RemoveArchiveAfterProvider {
                path: case.files.path().join("inbound/synthetic.eml.bin"),
            })
            .expect(1)
            .mount(&case.provider)
            .await;
        let source_id = super::super::workflow_id(case.request.receipt_id);
        assert!(matches!(
            case.summary
                .run_with(
                    case.request.clone(),
                    dbos::RunOptions {
                        workflow_id: Some(&source_id),
                        ..Default::default()
                    }
                )
                .await,
            Err(dbos::Error::Application(EmailSummaryError::Delivery(_)))
        ));
        let original = case.dbos.list_workflow_steps(&source_id).await.unwrap();
        let provider = original
            .iter()
            .find(|step| step.step_name == "summarize-gemini")
            .unwrap();
        assert!(provider.error.is_none());
        assert!(original
            .iter()
            .any(|step| step.step_name == "deliver-slack" && step.error.is_some()));
        let storage = cloud::FsStorage::new(case.files.path()).await.unwrap();
        storage
            .put("inbound/synthetic.eml", ARCHIVE, "message/rfc822")
            .await
            .unwrap();
        case.provider.reset().await;
        let mut command = case.command();
        command.after_failed_result = None;
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        let report = case.complete(&command).await;
        let recovered = case
            .dbos
            .list_workflow_steps(&report.target_workflow_id)
            .await
            .unwrap();
        assert_eq!(
            recovered
                .iter()
                .find(|step| step.step_name == "summarize-gemini")
                .unwrap()
                .output,
            provider.output
        );
        assert!(case.provider.received_requests().await.unwrap().is_empty());
        assert_eq!(case.slack.posted_messages().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn explicit_cancelled_recovery_preserves_confirmed_delivery_and_provider_checkpoint() {
    with_case(|case|async move {
        case.allow_success().await;
        let source_id=super::super::workflow_id(case.request.receipt_id);
        let gate=CheckpointGate::install_on(&case,&source_id,"dbos.operation_outputs","INSERT","NEW.function_name='deliver-slack'").await;
        case.summary.start_with(case.request.clone(),dbos::StartOptions {workflow_id:Some(&source_id),..Default::default()}).await.unwrap();
        wait_for(&case,"SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query ILIKE '%operation_outputs%' AND $1::text IS NOT NULL)",Uuid::now_v7()).await;
        assert_eq!(case.slack.posted_messages().len(),1);
        case.dbos.shutdown().await;
        let recorded:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dbos.operation_outputs WHERE workflow_uuid=$1 AND function_name='deliver-slack')").bind(&source_id).fetch_one(case.pg.pool()).await.unwrap();
        assert!(!recorded);
        gate.remove(&case).await;
        let client=dbos::Client::connect(dbos::ClientConfig {app_name:Some(workflows::dbos_engine::APP_NAME.into()),schema:workflows::dbos_engine::SYSTEM_SCHEMA.into(),..dbos::ClientConfig::new(case.url.clone())}).await.unwrap();
        let canceled=client.cancel(&source_id).await;
        client.close().await;
        canceled.unwrap();
        case.dbos.launch().await.unwrap();
        let source=read_record(&case.dbos,&source_id).await.unwrap().unwrap();
        assert_eq!(source.status,WorkflowStatus::Cancelled);
        let resumed_gate=CheckpointGate::install_on(&case,&source_id,"dbos.operation_outputs","INSERT","NEW.function_name='deliver-slack'").await;
        let mut command=case.command();command.after_failed_result=None;
        assert_eq!(case.submit(&command,true).await,StatusCode::ACCEPTED);
        let controller=case.dbos.retrieve_workflow::<RecoveryReport,RecoveryError>(&email_summary_recovery_id(command.operation_id)).unwrap();
        let report=tokio::time::timeout(Duration::from_secs(60),controller.result()).await.unwrap().unwrap();
        assert_eq!(report.action,Action::Resume);
        wait_for(&case,"SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query ILIKE '%operation_outputs%' AND $1::text IS NOT NULL)",command.operation_id).await;
        let mut next=case.command();next.from_recovery=Some(command.operation_id);next.after_failed_result=None;
        assert_eq!(case.submit(&next,true).await,StatusCode::CONFLICT);
        assert_eq!(ownership::current_operation(&case.pg,case.request.receipt_id).await.unwrap(),Some(command.operation_id));
        resumed_gate.unblock(&case).await;
        case.complete(&command).await;
        assert_eq!(report.target_workflow_id,source_id);
        assert_eq!(case.provider.received_requests().await.unwrap().len(),1);
        assert_eq!(case.slack.posted_messages().len(),1);
        assert_eq!(case.submit(&command,true).await,StatusCode::ACCEPTED);
        case.complete(&command).await;
        assert_eq!(case.slack.posted_messages().len(),1);
    }).await;
}

#[tokio::test]
async fn a_preexisting_child_without_the_recovery_claim_cannot_reopen_delivery() {
    with_case(|case| async move {
        case.failed_summary().await;
        let command = case.command();
        let source = super::super::workflow_id(case.request.receipt_id);
        let steps = case.dbos.list_workflow_steps(&source).await.unwrap();
        let stage = steps
            .iter()
            .find(|step| step.step_name == "summarize-gemini")
            .unwrap()
            .step_id;
        let child = email_summary_recovery_child(case.request.receipt_id, command.operation_id);
        case.dbos
            .fork_with::<EmailSummaryReport, EmailSummaryError>(
                dbos::Fork {
                    original_id: &source,
                    start_step: stage,
                    forked_id: Some(&child),
                },
                dbos::ForkOptions::default(),
            )
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let provider_calls = case.provider.received_requests().await.unwrap().len();
        assert_eq!(case.submit(&command, true).await, StatusCode::CONFLICT);
        assert_eq!(
            ownership::current_operation(&case.pg, case.request.receipt_id)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            case.provider.received_requests().await.unwrap().len(),
            provider_calls
        );
        let delivery_state: String =
            sqlx::query_scalar("SELECT state FROM email_delivery WHERE receipt_id=$1")
                .bind(case.request.receipt_id)
                .fetch_one(case.pg.pool())
                .await
                .unwrap();
        assert_eq!(delivery_state, store::email_deliveries::CONFIRMED);
        assert_eq!(case.slack.posted_messages().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn a_new_audited_operation_repairs_a_terminal_failed_recovery_child() {
    with_case(|case| async move {
        case.failed_summary().await;
        case.provider.reset().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&case.provider)
            .await;
        let first = case.command();
        assert_eq!(case.submit(&first, true).await, StatusCode::ACCEPTED);
        let first_report = case.complete(&first).await;
        assert_eq!(case.slack.posted_messages().len(), 2);
        case.provider.reset().await;
        case.allow_success().await;
        let mut next = case.command();
        next.from_recovery = Some(first.operation_id);
        assert_eq!(case.submit(&next, true).await, StatusCode::ACCEPTED);
        let next_report = case.complete(&next).await;
        assert_eq!(
            next_report.source_workflow_id,
            first_report.target_workflow_id
        );
        assert_eq!(case.slack.posted_messages().len(), 3);
        assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
        let receipts: i64 = sqlx::query_scalar("SELECT count(*) FROM email_receipt")
            .fetch_one(case.pg.pool())
            .await
            .unwrap();
        assert_eq!(receipts, 1);
        assert_eq!(
            ownership::current_operation(&case.pg, case.request.receipt_id)
                .await
                .unwrap(),
            Some(next.operation_id)
        );
    })
    .await;
}

#[test]
fn parked_recovery_is_operator_resumable_but_active_work_is_not_handoff_ready() {
    assert!(target_can_handoff(
        WorkflowStatus::MaxRecoveryAttemptsExceeded
    ));
    for status in [
        WorkflowStatus::Pending,
        WorkflowStatus::Enqueued,
        WorkflowStatus::Delayed,
    ] {
        assert!(!target_can_handoff(status));
    }
}

#[tokio::test]
async fn unknown_delivery_requires_native_resend_authorization_and_reuses_provider_output() {
    with_case_mode(true, |case| async move {
        case.allow_success().await;
        let slack = case.slack_http.as_ref().unwrap();
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(slack)
            .await;
        let id = super::super::workflow_id(case.request.receipt_id);
        let report = case
            .summary
            .run_with(
                case.request.clone(),
                dbos::RunOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(report.delivery_state, store::email_deliveries::UNKNOWN);
        let mut command = case.command();
        command.after_failed_result = None;
        assert_eq!(case.submit(&command, true).await, StatusCode::CONFLICT);
        let source = store::application_store::ApplicationStore::uninitialized();
        store::email_deliveries::authorize_resend(
            &source,
            case.request.receipt_id,
            "synthetic-reconciler",
        )
        .await
        .unwrap();
        slack.reset().await;
        for threaded in [false, true] {
            Mock::given(method("POST"))
                .and(move |request: &wiremock::Request| {
                    let body: serde_json::Value = request.body_json().unwrap();
                    body["channel"] == "C-SYNTHETIC"
                        && if threaded {
                            body["thread_ts"] == "123.4"
                        } else {
                            body.get("thread_ts").is_none()
                        }
                })
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"ok":true,"channel":"C-SYNTHETIC","ts":"123.4"}),
                ))
                .expect(1)
                .mount(slack)
                .await;
        }
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        let recovered = case.complete(&command).await;
        assert!(matches!(recovered.action, Action::ReconciledDelivery(_)));
        assert_eq!(case.provider.received_requests().await.unwrap().len(), 1);
        let delivered = slack.received_requests().await.unwrap();
        assert_eq!(delivered.len(), 2);
        let bodies: Vec<serde_json::Value> = delivered
            .iter()
            .map(|request| request.body_json().unwrap())
            .collect();
        let root = bodies
            .iter()
            .find(|body| body.get("thread_ts").is_none())
            .unwrap();
        let reply = bodies
            .iter()
            .find(|body| body["thread_ts"] == "123.4")
            .unwrap();
        assert_eq!(
            root["client_msg_id"],
            format!("summary-{}", case.request.receipt_id)
        );
        assert!(reply.get("client_msg_id").is_none());
        assert_ne!(root["text"], reply["text"]);
        assert_eq!(case.submit(&command, true).await, StatusCode::ACCEPTED);
        case.complete(&command).await;
        let replayed: Vec<serde_json::Value> = slack
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| request.body_json().unwrap())
            .collect();
        assert_eq!(replayed, bodies);
    })
    .await;
}

#[tokio::test]
async fn committed_child_without_fork_acknowledgement_is_retrieved_by_its_owned_operation() {
    with_case(|case|async move {
        case.failed_summary().await;
        let command=case.command();
        let gate=CheckpointGate::install(&case,command.operation_id,false).await;
        assert_eq!(case.submit(&command,true).await,StatusCode::ACCEPTED);
        wait_for(&case,"SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query ILIKE '%operation_outputs%' AND $1::text IS NOT NULL)",command.operation_id).await;
        assert_eq!(ownership::current_operation(&case.pg,case.request.receipt_id).await.unwrap(),Some(command.operation_id));
        let operation=read_record(&case.dbos,&email_summary_recovery_id(command.operation_id)).await.unwrap().unwrap();
        let bound=decode_recovery_input(&operation).unwrap();
        let Action::RetryProvider(step) = bound.action else { panic!("expected a provider-failure repair") };
        // Reconstruct the durable state of a committed fork whose acknowledgement
        // is missing: native admission and its immutable operation claim exist,
        // the exact child exists, and this controller has no fork checkpoint.
        case.dbos.fork_with::<EmailSummaryReport,EmailSummaryError>(dbos::Fork {original_id:&bound.source_workflow_id,start_step:step,forked_id:Some(&bound.target_workflow_id)},dbos::ForkOptions::default()).await.unwrap().result().await.unwrap();
        let acknowledged:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM dbos.operation_outputs WHERE workflow_uuid=$1 AND function_name='DBOS.forkWorkflow')").bind(email_summary_recovery_id(command.operation_id)).fetch_one(case.pg.pool()).await.unwrap();
        assert!(!acknowledged);
        gate.unblock(&case).await;
        let report=case.complete(&command).await;
        assert_eq!(report.target_workflow_id,bound.target_workflow_id);
        assert_eq!(case.provider.received_requests().await.unwrap().len(),1);
        assert_eq!(case.slack.posted_messages().len(),2);
        assert_eq!(case.submit(&command,true).await,StatusCode::ACCEPTED);
        case.complete(&command).await;
        assert_eq!(case.provider.received_requests().await.unwrap().len(),1);
        assert_eq!(case.slack.posted_messages().len(),2);
    }).await;
}

#[tokio::test]
async fn a_restored_receipt_owner_without_its_controller_history_cannot_reopen_delivery() {
    with_case(|case| async move {
        case.failed_summary().await;
        let missing_controller = Uuid::now_v7();
        // Model a partial public-row restore through the real native admission seam.
        // No DBOS status, inputs, outputs or checkpoints are manufactured.
        ownership::claim(
            &case.pg,
            case.request.receipt_id,
            missing_controller,
            None,
            &case.request.gemini.input_digest,
            ownership::DeliveryRule::PreserveConfirmed,
        )
        .await
        .unwrap();
        let mut command = case.command();
        command.from_recovery = Some(missing_controller);
        let provider_calls = case.provider.received_requests().await.unwrap().len();
        assert_eq!(case.submit(&command, true).await, StatusCode::CONFLICT);
        assert_eq!(
            ownership::current_operation(&case.pg, case.request.receipt_id)
                .await
                .unwrap(),
            Some(missing_controller)
        );
        let state: String =
            sqlx::query_scalar("SELECT state FROM email_delivery WHERE receipt_id=$1")
                .bind(case.request.receipt_id)
                .fetch_one(case.pg.pool())
                .await
                .unwrap();
        assert_eq!(state, store::email_deliveries::CONFIRMED);
        assert_eq!(
            case.provider.received_requests().await.unwrap().len(),
            provider_calls
        );
        assert_eq!(case.slack.posted_messages().len(), 1);
        let controller = email_summary_recovery_id(command.operation_id);
        assert!(case
            .dbos
            .list_workflows(&dbos::sysdb::types::WorkflowFilter {
                workflow_ids: vec![&controller],
                limit: Some(1),
                ..Default::default()
            })
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}
