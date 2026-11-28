//! The real caller wire through the authenticated DBOS Notation worker.

use std::sync::Arc;

use workflows::{MachineKind, StateMachineRuntime};
use workflows_service::dbos_ingress::{NotationIngress, WorkerIngress};
use workflows_service::dbos_notation::{self, Context};

use crate::support::{database, APP_VERSION};
use crate::DEADLINE;

async fn fork_from_last_failure(
    worker: &dbos::DBOS,
    workflow_id: &str,
) -> dbos::WorkflowHandle<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError> {
    let steps = worker.list_workflow_steps(workflow_id).await.unwrap();
    let restart_step = steps
        .iter()
        .rev()
        .find(|step| step.error.is_some())
        .or_else(|| steps.last())
        .expect("admitted transition has a checkpoint")
        .step_id;
    worker.fork(workflow_id, restart_step).await.unwrap()
}

struct PersistedNotation {
    database: Arc<crate::support::TestDatabase>,
    directory: Arc<tempfile::TempDir>,
    worker: dbos::DBOS,
    workflow: dbos_notation::NotationTransitionWorkflow,
    application_store: store::application_store::ApplicationStore,
    email: Arc<workflows::CapturingEmail>,
    id: uuid::Uuid,
    actor: uuid::Uuid,
    servers: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl PersistedNotation {
    async fn new(
        database: Arc<crate::support::TestDatabase>,
        directory: Arc<tempfile::TempDir>,
        worker: dbos::DBOS,
        servers: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    ) -> Self {
        let postgres = database.store();
        let application_store = store::application_store::ApplicationStore::uninitialized();
        let storage: Arc<dyn cloud::StorageService> =
            Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
        let (id, actor) = crate::persisted_fixture::seed(&postgres, storage.clone()).await;
        let email = Arc::new(workflows::CapturingEmail::default());
        let workflow = dbos_notation::register(
            &worker,
            Context::new(application_store.clone(), postgres, email.clone(), storage),
        )
        .unwrap();
        worker.launch().await.unwrap();
        Self {
            database,
            directory,
            worker,
            workflow,
            application_store,
            email,
            id,
            actor,
            servers,
        }
    }

    async fn transition(&self, kind: MachineKind, operation: dbos_notation::Operation) -> String {
        let handle = self
            .workflow
            .start_with(
                dbos_notation::TransitionRequest {
                    kind,
                    notation_id: self.id,
                    operation,
                },
                dbos::StartOptions::default(),
            )
            .await
            .unwrap();
        tokio::time::timeout(DEADLINE, handle.result())
            .await
            .expect("transition completes")
            .unwrap()
            .next_state
    }

    async fn start(&self, kind: MachineKind, spec: &str) {
        assert_eq!(
            self.transition(
                kind,
                dbos_notation::Operation::Start {
                    spec_yaml: spec.into(),
                    ephemeral: false,
                }
            )
            .await,
            "BEGIN"
        );
    }

    async fn signal(&self, kind: MachineKind, condition: &str, value: Option<String>) -> String {
        self.transition(
            kind,
            dbos_notation::Operation::Signal {
                condition: condition.into(),
                value,
                acting_person_id: Some(self.actor),
                ephemeral: false,
            },
        )
        .await
    }

    async fn events(&self) -> Vec<store::notation_events::NotationEvent> {
        store::notation_events::for_notation(&self.application_store, self.id)
            .await
            .unwrap()
    }
}

async fn with_persisted<F, Fut>(test: F)
where
    F: FnOnce(PersistedNotation) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Some(database) = database().await else {
        return;
    };
    let database = Arc::new(database);
    let directory = Arc::new(tempfile::tempdir().unwrap());
    let worker = dbos::DBOS::new(workflows::dbos_engine::config(&database.url, APP_VERSION));
    let servers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let postgres = database.store();
    let task_database = Arc::clone(&database);
    let task_directory = Arc::clone(&directory);
    let task_worker = worker.clone();
    let task_servers = Arc::clone(&servers);
    let result = tokio::spawn(store::postgres::PostgresDb::with_application_db(
        postgres,
        async move {
            let fixture =
                PersistedNotation::new(task_database, task_directory, task_worker, task_servers)
                    .await;
            test(fixture).await;
        },
    ))
    .await;
    worker.shutdown().await;
    let owned = std::mem::take(&mut *servers.lock().unwrap());
    for server in owned {
        server.abort();
        let _ = server.await;
    }
    let database = Arc::try_unwrap(database)
        .ok()
        .expect("all fixture consumers drained");
    database.close().await;
    drop(directory);
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("native application fixture admission: {error}"),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => panic!("owned persisted Notation task was cancelled: {error}"),
    }
}

#[tokio::test]
async fn persisted_review_hops_journal_the_actor_and_capture_both_notifications() {
    with_persisted(|fixture| async move {
        use views::brand::BrandKey;
        let notation = store::notations::find_by_id(&fixture.application_store, fixture.id)
            .await
            .unwrap()
            .unwrap();
        let brand = BrandKey::Vesta;
        use_review_brand(&fixture, notation.project_id, brand).await;
        for row in store::projects::participations_for_project(&fixture.application_store, notation.project_id)
            .await
            .unwrap()
        {
            store::projects::clear_dri(
                &fixture.application_store,
                row.id,
                store::projects::DriSide::Client,
            )
            .await
            .unwrap();
        }
        let lawyer = store::persons::create(
            &fixture.application_store,
            &store::persons::NewPerson::with_role(
                "Synthetic Reviewer",
                format!("reviewer-{}@example.com", uuid::Uuid::now_v7()),
                store::persons::Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        store::projects::designate_dri(
            &fixture.application_store,
            notation.project_id,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        store::projects::designate_dri(
            &fixture.application_store,
            notation.project_id,
            fixture.actor,
            store::projects::DriSide::Client,
        )
        .await
        .unwrap();
        fixture.start(MachineKind::Workflow,
            "BEGIN:\n  _: lawyer_review__draft\nlawyer_review__draft:\n  approved: client_review__draft\nclient_review__draft:\n  _: END\nEND: {}\n").await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "lawyer_review__draft"
        );
        assert_eq!(fixture.email.captured().len(), 1);
        let mail = &fixture.email.captured()[0];
        let origin = brand.public_home_href();
        let url = format!("{origin}/app/lawyer/notations/{}/review", fixture.id);
        assert!(mail.body.contains(&url));
        let html = mail.html_body.as_deref().unwrap();
        assert!(html.contains(&url));
        assert!(html.contains(&format!("{origin}/public/brand/vesta.png")));
        assert!(!html.contains("your-domain.example"));
        let branding = brand.resolve_branding(&views::brand::DEFAULT_BRANDING);
        assert!(html.contains(branding.firm_email));
        assert_eq!(
            mail.from,
            views::brand::sender_email_for_host(brand.canonical_host())
        );
        assert_eq!(
            fixture.email.captured()[0].subject,
            "A draft is ready for your review"
        );
        assert_eq!(
            fixture
                .signal(MachineKind::Workflow, "approved", None)
                .await,
            "client_review__draft"
        );
        let captured = fixture.email.captured();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[1].subject, "Your reviewed draft is ready");
        assert!(captured
            .iter()
            .all(|mail| mail.to.ends_with("@example.com")));
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "END"
        );
        let events = fixture.events().await;
        assert_eq!(events.len(), 3);
        assert!(events
            .iter()
            .all(|event| event.acting_person_id == fixture.actor && event.machine_kind == "workflow"));
        assert_eq!(events[1].condition, "approved");
        assert_eq!(events[1].from_state, "lawyer_review__draft");
        assert_eq!(events[1].to_state, "client_review__draft");
    }).await;
}

#[tokio::test]
async fn questionnaire_back_keeps_answers_and_appends_an_attributed_back_event() {
    with_persisted(|fixture| async move {
        let kind = MachineKind::Questionnaire;
        fixture.start(kind, "BEGIN:\n  _: first\nfirst:\n  _: second\nsecond:\n  _: third\nthird:\n  _: END\nEND: {}\n").await;
        assert_eq!(fixture.signal(kind, "_", None).await, "first");
        assert_eq!(
            fixture
                .signal(kind, "_", Some("synthetic answer".into()))
                .await,
            "second"
        );
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "first"
        );
        let events = fixture.events().await;
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1].payload.as_deref(),
            Some("{\"answer_value\":\"synthetic answer\"}")
        );
        assert_eq!(events[2].condition, "back");
        assert_eq!(events[2].from_state, "second");
        assert_eq!(events[2].to_state, "first");
        assert_eq!(events[2].acting_person_id, fixture.actor);
        assert!(fixture.email.captured().is_empty());
    }).await;
}

#[tokio::test]
async fn a_persisted_email_dispatch_is_captured_and_journaled() {
    with_persisted(|fixture| async move {
        fixture
            .start(
                MachineKind::Workflow,
                "BEGIN:\n  _: email_send__welcome\nemail_send__welcome:\n  _: END\nEND: {}\n",
            )
            .await;
        let payload = serde_json::to_string(&workflows::email::dispatch::EmailPayload::new(
            "Synthetic",
            "synthetic@example.com",
        ))
        .unwrap();
        assert_eq!(
            fixture
                .signal(MachineKind::Workflow, "_", Some(payload))
                .await,
            "email_send__welcome"
        );
        let captured = fixture.email.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].to, "synthetic@example.com");
        let events = fixture.events().await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].to_state, "email_send__welcome");
        assert!(
            !events[0]
                .payload
                .as_deref()
                .unwrap()
                .contains("synthetic@example.com"),
            "email recipient content stays out of the journal"
        );
    })
    .await;
}

#[derive(Default)]
struct RepairableEmail {
    available: std::sync::atomic::AtomicBool,
    captured: std::sync::Mutex<Vec<workflows::email::OutboundEmail>>,
}
#[async_trait::async_trait]
impl workflows::EmailService for RepairableEmail {
    async fn send(
        &self,
        email: workflows::email::OutboundEmail,
    ) -> Result<workflows::email::service::SendReceipt, workflows::email::service::EmailError> {
        if !self.available.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(workflows::email::service::EmailError::Transport(
                "synthetic unavailable".into(),
            ));
        }
        self.captured.lock().unwrap().push(email);
        Ok(workflows::email::service::SendReceipt::default())
    }
}

fn ephemeral_signal(
    notation: uuid::Uuid,
    value: Option<String>,
) -> dbos_notation::TransitionRequest {
    dbos_notation::TransitionRequest {
        kind: MachineKind::Workflow,
        notation_id: notation,
        operation: dbos_notation::Operation::Signal {
            condition: "_".into(),
            value,
            acting_person_id: None,
            ephemeral: true,
        },
    }
}
async fn repair_worker(
    database: &crate::support::TestDatabase,
    postgres: &store::postgres::PostgresDb,
    directory: &std::path::Path,
    email: Arc<RepairableEmail>,
) -> (dbos::DBOS, dbos_notation::NotationTransitionWorkflow) {
    let worker = dbos::DBOS::new(workflows::dbos_engine::config(&database.url, APP_VERSION));
    let workflow = dbos_notation::register(
        &worker,
        Context::new(
            store::application_store::ApplicationStore::uninitialized(),
            postgres.clone(),
            email,
            Arc::new(cloud::FsStorage::new(directory).await.unwrap()),
        ),
    )
    .unwrap();
    worker.launch().await.unwrap();
    (worker, workflow)
}

/// Regression: replay must not emit an obsolete predecessor's effects after
/// a different successful operation has already moved this same machine onward.
/// This is ordering, not exactly-once delivery: the first attempt sent no email.
#[tokio::test]
async fn an_unfinished_predecessor_owns_the_notation_through_repair() {
    let Some(database) = database().await else {
        return;
    };
    let postgres = database.store();
    let directory = tempfile::tempdir().unwrap();
    let email = Arc::new(RepairableEmail::default());
    let (worker, workflow) =
        repair_worker(&database, &postgres, directory.path(), email.clone()).await;
    let notation = uuid::Uuid::now_v7();
    workflow
        .run(dbos_notation::TransitionRequest {
            kind: MachineKind::Workflow,
            notation_id: notation,
            operation: dbos_notation::Operation::Start {
                spec_yaml:
                    "BEGIN:\n  _: email_send__welcome\nemail_send__welcome:\n  _: END\nEND: {}\n"
                        .into(),
                ephemeral: true,
            },
        })
        .await
        .unwrap();
    let logical = format!("failed-predecessor-{notation}");
    let failed = workflow
        .start_with(
            ephemeral_signal(
                notation,
                Some(
                    serde_json::to_string(&workflows::email::dispatch::EmailPayload::new(
                        "Synthetic",
                        "synthetic@example.com",
                    ))
                    .unwrap(),
                ),
            ),
            dbos::StartOptions {
                workflow_id: Some(&logical),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(tokio::time::timeout(DEADLINE, failed.result())
        .await
        .unwrap()
        .is_err());
    assert!(
        email.captured.lock().unwrap().is_empty(),
        "no earlier accepted external effect to duplicate"
    );
    assert_eq!(
        store::notation_machine_state::read(&postgres, notation, "workflow")
            .await
            .unwrap()
            .unwrap()
            .current_state,
        "email_send__welcome"
    );
    // Durable ownership survives release of the live advisory lane.
    let successor = workflow.run(ephemeral_signal(notation, None)).await;
    assert!(matches!(
        successor,
        Err(dbos::Error::Application(
            dbos_notation::NotationTransitionError::LaneBusy { .. }
        ))
    ));
    email
        .available
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let repaired = fork_from_last_failure(&worker, &logical).await;
    let result = tokio::time::timeout(DEADLINE, repaired.result())
        .await
        .unwrap();
    assert_eq!(result.unwrap().next_state, "email_send__welcome");
    assert_eq!(
        email.captured.lock().unwrap().len(),
        1,
        "the original rejected send happens once after repair"
    );
    let successor = workflow
        .run(ephemeral_signal(notation, None))
        .await
        .unwrap();
    assert_eq!(successor.next_state, "END");
    assert_eq!(
        store::notation_machine_state::read(&postgres, notation, "workflow")
            .await
            .unwrap()
            .unwrap()
            .current_state,
        "END"
    );
    worker.shutdown().await;
    database.close().await;
}

#[tokio::test]
async fn pending_operation_uniqueness_and_completion_are_atomic_under_contention() {
    use store::notation_machine_state::operations::{self, Admission, OperationError};
    let Some(database) = database().await else {
        return;
    };
    let pg = database.store();
    let notation = uuid::Uuid::now_v7();
    let barrier = Arc::new(tokio::sync::Barrier::new(8));
    let mut writers = Vec::new();
    for _ in 0..8 {
        let pg = pg.clone();
        let barrier = barrier.clone();
        writers.push(tokio::spawn(async move {
            let id = uuid::Uuid::now_v7();
            barrier.wait().await;
            (id, operations::admit(&pg, id, notation, "same-input").await)
        }));
    }
    let mut winner = None;
    let mut refused = 0;
    for writer in writers {
        let (id, result) = writer.await.unwrap();
        match result {
            Ok(Admission::Pending { started: false }) => {
                assert!(winner.replace(id).is_none());
            }
            Err(OperationError::Busy) => refused += 1,
            other => panic!("unexpected admission: {other:?}"),
        }
    }
    assert_eq!(refused, 7);
    let winner = winner.unwrap();
    assert_eq!(
        operations::admit(&pg, winner, notation, "same-input")
            .await
            .unwrap(),
        Admission::Pending { started: false }
    );
    assert!(matches!(
        operations::admit(&pg, winner, notation, "changed-input").await,
        Err(OperationError::Conflict)
    ));
    operations::mark_started(&pg, winner).await.unwrap();
    operations::discard_unstarted(&pg, winner).await.unwrap();
    let successor = uuid::Uuid::now_v7();
    assert!(matches!(
        operations::admit(&pg, successor, notation, "next").await,
        Err(OperationError::Busy)
    ));
    let independent = uuid::Uuid::now_v7();
    assert!(matches!(
        operations::admit(&pg, independent, uuid::Uuid::now_v7(), "other").await,
        Ok(Admission::Pending { .. })
    ));
    let result = serde_json::json!({"next_state":"B"});
    operations::complete(&pg, winner, &result).await.unwrap();
    operations::complete(&pg, winner, &result).await.unwrap();
    assert!(matches!(
        operations::complete(&pg, winner, &serde_json::json!({"next_state":"C"})).await,
        Err(OperationError::Conflict)
    ));
    assert_eq!(
        operations::admit(&pg, successor, notation, "next")
            .await
            .unwrap(),
        Admission::Pending { started: false }
    );
    assert_eq!(
        operations::admit(&pg, winner, notation, "same-input")
            .await
            .unwrap(),
        Admission::Completed(result)
    );
    operations::discard_unstarted(&pg, successor).await.unwrap();
    assert!(matches!(
        operations::admit(&pg, uuid::Uuid::now_v7(), notation, "replacement").await,
        Ok(Admission::Pending { started: false })
    ));
    database.close().await;
}

#[tokio::test]
async fn invalid_preflight_does_not_leave_a_pending_owner_or_cached_machine_read() {
    with_persisted(|fixture| async move {
        let invalid = fixture
            .workflow
            .run(dbos_notation::TransitionRequest {
                kind: MachineKind::Workflow,
                notation_id: fixture.id,
                operation: dbos_notation::Operation::Start {
                    spec_yaml: "not: [a valid machine".into(),
                    ephemeral: false,
                },
            })
            .await;
        assert!(invalid.is_err());
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notation_transition WHERE notation_id=$1 AND result IS NULL",
        )
        .bind(fixture.id)
        .fetch_one(fixture.database.store().pool())
        .await
        .unwrap();
        assert_eq!(pending, 0);
        fixture
            .start(MachineKind::Workflow, "BEGIN:\n  _: END\nEND: {}\n")
            .await;
        let invalid = fixture
            .workflow
            .run(dbos_notation::TransitionRequest {
                kind: MachineKind::Workflow,
                notation_id: fixture.id,
                operation: dbos_notation::Operation::Signal {
                    condition: "not-declared".into(),
                    value: None,
                    acting_person_id: None,
                    ephemeral: false,
                },
            })
            .await;
        assert!(invalid.is_err());
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM notation_transition WHERE notation_id=$1 AND result IS NULL",
        )
        .bind(fixture.id)
        .fetch_one(fixture.database.store().pool())
        .await
        .unwrap();
        assert_eq!(pending, 0);
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "END"
        );
    })
    .await;
}

/// Mark-started fails outside a DBOS step. The anchor must preserve the first
/// logical UUID when the repair forks from the last recorded step.
#[tokio::test]
async fn a_live_preflight_failure_repairs_under_the_original_operation_identity() {
    with_persisted(|fixture| async move {
            sqlx::query(
            "ALTER TABLE notation_transition ADD CONSTRAINT synthetic_mark_denied CHECK (NOT started)",
        )
        .execute(fixture.database.store().pool())
        .await
        .unwrap();
            let logical = format!("preflight-failure-{}", fixture.id);
            let handle = fixture
                .workflow
                .start_with(
                    dbos_notation::TransitionRequest {
                        kind: MachineKind::Workflow,
                        notation_id: fixture.id,
                        operation: dbos_notation::Operation::Start {
                            spec_yaml: "BEGIN:\n  _: END\nEND: {}\n".into(),
                            ephemeral: false,
                        },
                    },
                    dbos::StartOptions {
                        workflow_id: Some(&logical),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert!(tokio::time::timeout(DEADLINE, handle.result())
                .await
                .unwrap()
                .is_err());
            let original: uuid::Uuid = sqlx::query_scalar(
                "SELECT id FROM notation_transition WHERE notation_id=$1 AND result IS NULL",
            )
            .bind(fixture.id)
            .fetch_one(fixture.database.store().pool())
            .await
            .unwrap();
            sqlx::query("ALTER TABLE notation_transition DROP CONSTRAINT synthetic_mark_denied")
                .execute(fixture.database.store().pool())
                .await
                .unwrap();
            let repaired = fork_from_last_failure(&fixture.worker, &logical).await;
            assert_eq!(
                tokio::time::timeout(DEADLINE, repaired.result())
                    .await
                    .unwrap()
                    .unwrap()
                    .next_state,
                "BEGIN"
            );
            let rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM notation_transition WHERE notation_id=$1")
                    .bind(fixture.id)
                    .fetch_one(fixture.database.store().pool())
                    .await
                    .unwrap();
            assert_eq!(rows, 1);
            let finished: bool =
                sqlx::query_scalar("SELECT result IS NOT NULL FROM notation_transition WHERE id=$1")
                    .bind(original)
                    .fetch_one(fixture.database.store().pool())
                    .await
                    .unwrap();
            assert!(finished);
    })
    .await;
}

async fn notation_project(fixture: &PersistedNotation) -> uuid::Uuid {
    store::notations::find_by_id(&fixture.application_store, fixture.id)
        .await
        .unwrap()
        .unwrap()
        .project_id
}

async fn review_lawyer(fixture: &PersistedNotation, name: &str) -> store::persons::Person {
    store::persons::create(
        &fixture.application_store,
        &store::persons::NewPerson::with_role(
            name,
            format!("review-{}@example.com", uuid::Uuid::now_v7()),
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn completing_the_firm_signature_state_closes_the_persisted_matter() {
    with_persisted(|mut fixture| async move {
        let project = notation_project(&fixture).await;
        let signer = review_lawyer(&fixture, "Synthetic Signer").await;
        let storage: Arc<dyn cloud::StorageService> = Arc::new(cloud::FsStorage::new(fixture.directory.path()).await.unwrap());
        store::assets::file_revision(
            &fixture.application_store,&storage,
            &store::documents::IngestArgs {project_id:project,source:"upload",filename:"closing.pdf",kind:"offboarding",content_type:"application/pdf",
                description:None,visibility:"internal",secondary_storage_key:None},
            &store::documents::DocumentIdentity {slug:Some("closing-letter"),..Default::default()},
            include_bytes!("../../../cli/testdata/synthetic-initial-case-assessment.pdf"),
        ).await.unwrap();
        fixture.actor = signer.id;
        fixture.start(MachineKind::Workflow,
            "BEGIN:\n  _: firm_signature__closing_letter\nfirm_signature__closing_letter:\n  signed: END\nEND: {}\n").await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "firm_signature__closing_letter"
        );
        assert_eq!(
            store::projects::find_by_id(&fixture.application_store, project)
                .await
                .unwrap()
                .unwrap()
                .status,
            "open"
        );
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "signed", None).await,
            "END"
        );
        let closed = store::projects::find_by_id(&fixture.application_store, project)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(closed.status, "closed");
        assert!(closed.closed_at.is_some());
        assert_eq!(fixture.events().await.len(), 2);
        assert!(fixture.email.captured().is_empty());
    }).await;
}

async fn use_review_brand(
    fixture: &PersistedNotation,
    project: uuid::Uuid,
    brand: views::brand::BrandKey,
) {
    let db = &fixture.application_store;
    let (firm_id, _) = store::test_support::seed_firm_with_new_brand(db, brand.as_str()).await;
    store::projects::update_project(
        db,
        project,
        &store::projects::UpdateProjectCommand {
            firm_id: Some(firm_id),
            brand: Some(brand.as_str().to_owned()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn review_notification_is_skipped_if_the_project_brand_has_no_host() {
    with_persisted(|fixture| async move {
        let project = notation_project(&fixture).await;
        let lawyer = review_lawyer(&fixture, "Synthetic Reviewer").await;
        store::projects::designate_dri(
            &fixture.application_store,
            project,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        fixture.start(
            MachineKind::Workflow,
            "BEGIN:\n  _: lawyer_review__draft\nlawyer_review__draft:\n  approved: END\nEND: {}\n",
        ).await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "lawyer_review__draft"
        );
        assert!(fixture.email.captured().is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_demoted_review_dri_falls_back_to_a_current_lawyer() {
    with_persisted(|fixture| async move {
        let project = notation_project(&fixture).await;
        use_review_brand(&fixture, project, views::brand::BrandKey::Vesta).await;
        let stale = review_lawyer(&fixture, "Former Reviewer").await;
        store::projects::designate_dri(
            &fixture.application_store,
            project,
            stale.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
        store::persons::set_role(
            &fixture.application_store,
            stale.id,
            store::persons::Role::Client,
        )
        .await
        .unwrap();
        let current = review_lawyer(&fixture, "Current Reviewer").await;
        store::projects::add_participation(
            &fixture.application_store,
            project,
            current.id,
            store::projects::participation_for_role(store::persons::Role::Lawyer),
        )
        .await
        .unwrap();
        fixture
        .start(
            MachineKind::Workflow,
            "BEGIN:\n  _: lawyer_review__draft\nlawyer_review__draft:\n  approved: END\nEND: {}\n",
        )
        .await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "lawyer_review__draft"
        );
        let captured = fixture.email.captured();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].to, current.email);
        assert_ne!(captured[0].to, stale.email);
        assert_eq!(captured[0].subject, "A draft is ready for your review");
    })
    .await;
}

#[tokio::test]
async fn a_signal_without_a_machine_fails_without_a_journal_or_send() {
    with_persisted(|fixture| async move {
        let handle = fixture
            .workflow
            .start_with(
                dbos_notation::TransitionRequest {
                    kind: MachineKind::Workflow,
                    notation_id: fixture.id,
                    operation: dbos_notation::Operation::Signal {
                        condition: "_".into(),
                        value: None,
                        acting_person_id: Some(fixture.actor),
                        ephemeral: false,
                    },
                },
                dbos::StartOptions::default(),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(DEADLINE, handle.result())
            .await
            .expect("the missing machine fails promptly");
        assert!(result.is_err());
        assert!(fixture.events().await.is_empty());
        assert!(fixture.email.captured().is_empty());
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM notation_machine_state WHERE notation_id = $1",
        )
        .bind(fixture.id)
        .fetch_one(fixture.database.store().pool())
        .await
        .unwrap();
        assert_eq!(count, 0);
    })
    .await;
}

#[tokio::test]
async fn a_back_event_written_before_its_state_move_keeps_later_back_navigation_correct() {
    with_persisted(|fixture| async move {
        let kind = MachineKind::Questionnaire;
        fixture.start(kind, "BEGIN:\n  _: first\nfirst:\n  _: second\nsecond:\n  _: done\ndone:\n  _: END\nEND: {}\n").await;
        assert_eq!(fixture.signal(kind, "_", None).await, "first");
        assert_eq!(
            fixture
                .signal(kind, "_", Some("synthetic answer".into()))
                .await,
            "second"
        );
        // A process loss after the append but before the machine CAS leaves
        // exactly these durable facts for the recovered command to read.
        store::notation_events::append_event(
            &fixture.application_store,
            store::notation_events::TransitionRecord {
                notation_id: fixture.id,
                acting_person_id: Some(fixture.actor),
                machine_kind: "questionnaire",
                from_state: "second",
                to_state: "first",
                condition: "back",
                payload_json: None,
                recorded_at: &chrono::Utc::now().to_rfc3339(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "first"
        );
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "BEGIN"
        );
        let events = fixture.events().await;
        assert_eq!(events.last().unwrap().to_state, "BEGIN");
        assert!(events
            .iter()
            .any(|event| event.payload.as_deref() == Some("{\"answer_value\":\"synthetic answer\"}")));
    }).await;
}

#[tokio::test]
async fn back_keeps_begin_and_completed_questionnaires_in_place() {
    with_persisted(|fixture| async move {
        let kind = MachineKind::Questionnaire;
        fixture
            .start(
                kind,
                "BEGIN:\n  _: first\nfirst:\n  _: done\ndone:\n  _: END\nEND: {}\n",
            )
            .await;
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "BEGIN"
        );
        assert!(fixture.events().await.is_empty());
        assert_eq!(fixture.signal(kind, "_", None).await, "first");
        assert_eq!(
            fixture
                .signal(kind, "_", Some("synthetic answer".into()))
                .await,
            "done"
        );
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "done"
        );
        assert_eq!(fixture.signal(kind, "_", None).await, "END");
        assert_eq!(
            fixture
                .transition(
                    kind,
                    dbos_notation::Operation::Back {
                        acting_person_id: fixture.actor
                    }
                )
                .await,
            "END"
        );
        assert_eq!(fixture.events().await.len(), 3);
    })
    .await;
}

#[tokio::test]
async fn dbos_approval_accepts_a_retry_and_publishes_the_rendered_state_once() {
    with_persisted(|fixture| async move {
        fixture
            .start(
                MachineKind::Workflow,
                "BEGIN:\n  _: lawyer_review\nlawyer_review:\n  approved: generate_pdf__letter\ngenerate_pdf__letter:\n  pdf_persisted: END\nEND: {}\n",
            )
            .await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "lawyer_review"
        );
        let notation = store::notations::update_state(&fixture.application_store, fixture.id, "lawyer_review")
            .await
            .unwrap();
        let postgres = fixture.database.store();
        let (runtime, server) = approval_runtime(&fixture, postgres.clone()).await;
        let storage_key = store::notations::document_pdf_storage_key(fixture.id);
        let payload = serde_json::to_string(&workflows::DocumentPayload::Typst {
            storage_key: storage_key.clone(),
            typst_source: "Reviewed synthetic document".into(),
        })
        .unwrap();
        let review_revision = notation.updated_at.timestamp_micros();
        let actor = workflows::SignalContext {
            acting_person_id: fixture.actor,
        };
        assert_stale_approval_fails(&fixture, &runtime, review_revision, &payload, actor).await;
        let lane = store::notation_machine_state::acquire_lane(&postgres, fixture.id)
            .await
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    runtime.approve(fixture.id, review_revision, &payload, Some(actor)),
                )
                .await
                .expect("submission responds while the transition lane is held")
                .unwrap(),
                None
            );
        }
        assert_eq!(
            runtime
                .approval_progress(fixture.id, review_revision)
                .await
                .unwrap(),
            Some(workflows::ApprovalProgress::Pending)
        );
        drop(lane);
        let run_id = format!("NotationApproval:{}:{review_revision}", fixture.id);
        let handle = fixture.worker
            .retrieve_workflow::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(&run_id)
            .unwrap();
        assert_eq!(
            tokio::time::timeout(DEADLINE, handle.result())
                .await
                .unwrap()
                .unwrap()
                .next_state,
            "generate_pdf__letter"
        );
        assert_eq!(
            runtime
                .approval_progress(fixture.id, review_revision)
                .await
                .unwrap(),
            Some(workflows::ApprovalProgress::Ready)
        );
        let persisted = store::notations::find_by_id(&fixture.application_store, fixture.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.state, "generate_pdf__letter");
        let storage = cloud::FsStorage::new(fixture.directory.path())
            .await
            .unwrap();
        let document = cloud::StorageService::get(&storage, &storage_key)
            .await
            .unwrap();
        assert!(document.bytes.starts_with(b"%PDF-"));
        assert_eq!(
            fixture
                .events()
                .await
                .iter()
                .filter(|event| event.condition == "approved")
                .count(),
            1
        );
        server.abort();
    }).await;
}

#[tokio::test]
async fn dbos_approval_that_cannot_render_fails_at_lawyer_review_without_parking() {
    with_persisted(|fixture| async move {
        fixture
            .start(
                MachineKind::Workflow,
                "BEGIN:\n  _: lawyer_review\nlawyer_review:\n  approved: generate_pdf__letter\ngenerate_pdf__letter:\n  pdf_persisted: END\nEND: {}\n",
            )
            .await;
        assert_eq!(
            fixture.signal(MachineKind::Workflow, "_", None).await,
            "lawyer_review"
        );
        let notation = store::notations::update_state(&fixture.application_store, fixture.id, "lawyer_review")
            .await
            .unwrap();
        let postgres = fixture.database.store();
        let (runtime, server) = approval_runtime(&fixture, postgres.clone()).await;
        let storage_key = store::notations::document_pdf_storage_key(fixture.id);
        // Typst refuses an unterminated code block, so the render fails.
        let payload = serde_json::to_string(&workflows::DocumentPayload::Typst {
            storage_key: storage_key.clone(),
            typst_source: "#let broken = (".into(),
        })
        .unwrap();
        let review_revision = notation.updated_at.timestamp_micros();
        runtime
            .approve(
                fixture.id,
                review_revision,
                &payload,
                Some(workflows::SignalContext {
                    acting_person_id: fixture.actor,
                }),
            )
            .await
            .unwrap();
        let run_id = format!("NotationApproval:{}:{review_revision}", fixture.id);
        let handle = fixture.worker
            .retrieve_workflow::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(&run_id)
            .unwrap();
        let failure = tokio::time::timeout(DEADLINE, handle.result())
            .await
            .unwrap()
            .unwrap_err()
            .to_string();
        assert!(failure.contains("dispatch"), "{failure}");
        assert_eq!(
            runtime
                .approval_progress(fixture.id, review_revision)
                .await
                .unwrap(),
            Some(workflows::ApprovalProgress::Failed)
        );
        let machine = store::notation_machine_state::read(&postgres, fixture.id, "workflow")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(machine.current_state, "lawyer_review", "the run never parks at the render step");
        let persisted = store::notations::find_by_id(&fixture.application_store, fixture.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted.state, "lawyer_review");
        let storage = cloud::FsStorage::new(fixture.directory.path()).await.unwrap();
        assert!(cloud::StorageService::get(&storage, &storage_key).await.is_err());
        assert!(fixture
            .events()
            .await
            .iter()
            .all(|event| event.condition != "approved"));
        server.abort();
    }).await;
}

async fn assert_stale_approval_fails(
    fixture: &PersistedNotation,
    runtime: &workflows::DbosNotationRuntime,
    review_revision: i64,
    payload: &str,
    actor: workflows::SignalContext,
) {
    let stale_revision = review_revision - 1;
    runtime
        .approve(fixture.id, stale_revision, payload, Some(actor))
        .await
        .unwrap();
    let stale_run = format!("NotationApproval:{}:{stale_revision}", fixture.id);
    let stale_handle = fixture.worker
        .retrieve_workflow::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(&stale_run)
        .unwrap();
    assert!(tokio::time::timeout(DEADLINE, stale_handle.result())
        .await
        .unwrap()
        .is_err());
    assert_eq!(
        runtime
            .approval_progress(fixture.id, stale_revision)
            .await
            .unwrap(),
        Some(workflows::ApprovalProgress::Failed)
    );
}

async fn approval_runtime(
    fixture: &PersistedNotation,
    postgres: store::postgres::PostgresDb,
) -> (workflows::DbosNotationRuntime, tokio::task::AbortHandle) {
    let router = workflows_service::health::router_with_dbos_ingress(
        WorkerIngress {
            notation: Some(NotationIngress::new(
                "synthetic-worker-token".into(),
                fixture.worker.clone(),
                postgres,
                fixture.workflow.clone(),
            )),
            ..WorkerIngress::default()
        },
        None,
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let abort = server.abort_handle();
    fixture.servers.lock().unwrap().push(server);
    let runtime =
        workflows::DbosNotationRuntime::new(origin, Some("synthetic-worker-token".into()));
    (runtime, abort)
}
