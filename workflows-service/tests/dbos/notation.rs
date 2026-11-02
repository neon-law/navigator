//! The real caller wire through the authenticated DBOS Notation worker.

use std::sync::Arc;

use workflows::{MachineKind, StateMachineRuntime, StateName, WorkflowSpec};
use workflows_service::dbos_ingress::{NotationIngress, WorkerIngress};
use workflows_service::dbos_notation::{self, Context};

use crate::support::{database, APP_VERSION, DEADLINE};

async fn assert_read_boundary(origin: &str, notation_id: uuid::Uuid) {
    let current = format!("{origin}/notation/{notation_id}/workflow_current_state");
    let http = reqwest::Client::new();
    assert_eq!(
        http.post(&current).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.post(&current)
            .bearer_auth("synthetic-worker-token")
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::BAD_REQUEST,
        "state reads preserve the bodyless wire contract"
    );
}

#[tokio::test]
async fn the_authenticated_caller_keeps_both_notation_machines_independent() {
    let Some(db) = database().await else { return };
    let postgres = db.store().await;
    let surreal = store::test_support::mem_surreal().await;
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap());
    let worker = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_notation::register(
        &worker,
        Context::new(
            surreal.clone(),
            postgres.clone(),
            Arc::new(workflows::CapturingEmail::default()),
            storage,
        ),
    )
    .unwrap();
    worker.launch().await.unwrap();
    let router = workflows_service::health::router_with_dbos_ingress(WorkerIngress {
        notation: Some(NotationIngress::new(
            "synthetic-worker-token".into(),
            surreal,
            postgres,
            workflow,
        )),
        ..WorkerIngress::default()
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let notation_id = uuid::Uuid::now_v7();
    assert_read_boundary(&origin, notation_id).await;
    let runtime =
        workflows::DbosNotationRuntime::new(origin, Some("synthetic-worker-token".into()));
    let spec = WorkflowSpec::from_yaml("BEGIN:\n  _: END\nEND: {}\n").unwrap();
    tokio::time::timeout(DEADLINE, async {
        assert_eq!(
            runtime
                .current_state(MachineKind::Workflow, notation_id)
                .await,
            None
        );
        for kind in [MachineKind::Questionnaire, MachineKind::Workflow] {
            runtime
                .start_ephemeral(kind, notation_id, &spec)
                .await
                .unwrap();
            assert_eq!(
                runtime.current_state(kind, notation_id).await,
                Some(StateName::begin())
            );
        }
        assert_eq!(
            runtime
                .signal_ephemeral(MachineKind::Questionnaire, notation_id, "_", None)
                .await
                .unwrap(),
            StateName::end()
        );
        assert_eq!(
            runtime
                .current_state(MachineKind::Workflow, notation_id)
                .await,
            Some(StateName::begin()),
            "the questionnaire cannot advance the workflow machine"
        );
        runtime
            .start_ephemeral(MachineKind::Questionnaire, notation_id, &spec)
            .await
            .unwrap();
        assert_eq!(
            runtime
                .current_state(MachineKind::Questionnaire, notation_id)
                .await,
            Some(StateName::end()),
            "a duplicate start does not reset an advanced machine"
        );
        assert_eq!(
            runtime
                .signal_ephemeral(MachineKind::Workflow, notation_id, "_", None)
                .await
                .unwrap(),
            StateName::end()
        );
    })
    .await
    .expect("the authenticated Notation transitions finish");
    worker.shutdown().await;
    server.abort();
}

#[tokio::test]
async fn a_pending_notation_start_recovers_after_the_executor_stops() {
    let Some(db) = database().await else { return };
    let postgres = db.store().await;
    let dir = tempfile::tempdir().unwrap();
    let context = Context::new(
        store::test_support::mem_surreal().await,
        postgres.clone(),
        Arc::new(workflows::CapturingEmail::default()),
        Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap()),
    );
    let notation_id = uuid::Uuid::now_v7();
    let lane = store::notation_machine_state::acquire_lane(&postgres, notation_id)
        .await
        .unwrap();
    let worker = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = dbos_notation::register(&worker, context.clone()).unwrap();
    worker.launch().await.unwrap();
    let request = dbos_notation::TransitionRequest {
        kind: MachineKind::Workflow,
        notation_id,
        operation: dbos_notation::Operation::Start {
            spec_yaml: "BEGIN:\n  _: END\nEND: {}\n".into(),
            ephemeral: true,
        },
    };
    workflow
        .start_with(
            request,
            dbos::StartOptions {
                workflow_id: Some("synthetic-notation-recovery"),
                ..dbos::StartOptions::default()
            },
        )
        .await
        .unwrap();
    // The held lane prevents the first worker from changing machine state.
    worker.shutdown().await;
    assert!(
        store::notation_machine_state::read(&postgres, notation_id, "workflow")
            .await
            .unwrap()
            .is_none()
    );
    lane.commit().await.unwrap();
    let resumed = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    dbos_notation::register(&resumed, context).unwrap();
    resumed.launch().await.unwrap();
    let report = tokio::time::timeout(
        DEADLINE,
        resumed
            .retrieve_workflow::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(
                "synthetic-notation-recovery",
            )
            .unwrap()
            .result(),
    )
    .await
    .expect("the pending Notation start recovers")
    .unwrap();
    assert_eq!(report.next_state, "BEGIN");
    assert_eq!(
        store::notation_machine_state::read(&postgres, notation_id, "workflow")
            .await
            .unwrap()
            .unwrap()
            .current_state,
        "BEGIN"
    );
    resumed.shutdown().await;
}

struct PersistedNotation {
    database: crate::support::TestDatabase,
    _directory: tempfile::TempDir,
    worker: dbos::DBOS,
    workflow: dbos_notation::NotationTransitionWorkflow,
    surreal: store::surreal::SurrealDb,
    email: Arc<workflows::CapturingEmail>,
    id: uuid::Uuid,
    actor: uuid::Uuid,
}

impl PersistedNotation {
    async fn new() -> Option<Self> {
        let database = database().await?;
        let postgres = database.store().await;
        let surreal = store::test_support::mem_surreal().await;
        let id = store::test_support::seed_notation(&surreal).await;
        let actor = store::notations::find_by_id(&surreal, id)
            .await
            .unwrap()
            .unwrap()
            .person_id;
        let directory = tempfile::tempdir().unwrap();
        let email = Arc::new(workflows::CapturingEmail::default());
        let worker = dbos::DBOS::new(workflows::dbos_engine::config(&database.url, APP_VERSION));
        let workflow = dbos_notation::register(
            &worker,
            Context::new(
                surreal.clone(),
                postgres,
                email.clone(),
                Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap()),
            ),
        )
        .unwrap();
        worker.launch().await.unwrap();
        Some(Self {
            database,
            _directory: directory,
            worker,
            workflow,
            surreal,
            email,
            id,
            actor,
        })
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
        store::notation_events::for_notation(&self.surreal, self.id)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn persisted_review_hops_journal_the_actor_and_capture_both_notifications() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
    let notation = store::notations::find_by_id(&fixture.surreal, fixture.id)
        .await
        .unwrap()
        .unwrap();
    for row in store::projects::participations_for_project(&fixture.surreal, notation.project_id)
        .await
        .unwrap()
    {
        store::projects::clear_dri_in_surreal(
            &fixture.surreal,
            row.id,
            store::projects::DriSide::Client,
        )
        .await
        .unwrap();
    }
    let lawyer = store::persons::create(
        &fixture.surreal,
        &store::persons::NewPerson::with_role(
            "Synthetic Reviewer",
            format!("reviewer-{}@example.com", uuid::Uuid::now_v7()),
            store::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::designate_dri_in_surreal(
        &fixture.surreal,
        notation.project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    store::projects::designate_dri_in_surreal(
        &fixture.surreal,
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
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn questionnaire_back_keeps_answers_and_appends_an_attributed_back_event() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn a_persisted_email_dispatch_is_captured_and_journaled() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
    fixture.worker.shutdown().await;
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
            store::test_support::mem_surreal().await,
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
    let postgres = database.store().await;
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
    let repaired = worker
        .fork::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(
            &logical,
            dbos::ForkFrom::LastFailure,
        )
        .await
        .unwrap();
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
}

#[tokio::test]
async fn pending_operation_uniqueness_and_completion_are_atomic_under_contention() {
    use store::notation_machine_state::operations::{self, Admission, OperationError};
    let Some(database) = database().await else {
        return;
    };
    let pg = database.store().await;
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
}

#[tokio::test]
async fn invalid_preflight_does_not_leave_a_pending_owner_or_cached_machine_read() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
    .fetch_one(&fixture.database.pool)
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
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();
    assert_eq!(pending, 0);
    assert_eq!(
        fixture.signal(MachineKind::Workflow, "_", None).await,
        "END"
    );
    fixture.worker.shutdown().await;
}

/// Mark-started fails outside a DBOS step. The anchor must preserve the first
/// logical UUID even when `LastFailure` falls back to its last recorded step.
#[tokio::test]
async fn a_live_preflight_failure_repairs_under_the_original_operation_identity() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
    sqlx::query(
        "ALTER TABLE notation_transition ADD CONSTRAINT synthetic_mark_denied CHECK (NOT started)",
    )
    .execute(&fixture.database.pool)
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
    .fetch_one(&fixture.database.pool)
    .await
    .unwrap();
    sqlx::query("ALTER TABLE notation_transition DROP CONSTRAINT synthetic_mark_denied")
        .execute(&fixture.database.pool)
        .await
        .unwrap();
    let repaired = fixture
        .worker
        .fork::<dbos_notation::TransitionReport, dbos_notation::NotationTransitionError>(
            &logical,
            dbos::ForkFrom::LastFailure,
        )
        .await
        .unwrap();
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
            .fetch_one(&fixture.database.pool)
            .await
            .unwrap();
    assert_eq!(rows, 1);
    let finished: bool =
        sqlx::query_scalar("SELECT result IS NOT NULL FROM notation_transition WHERE id=$1")
            .bind(original)
            .fetch_one(&fixture.database.pool)
            .await
            .unwrap();
    assert!(finished);
    fixture.worker.shutdown().await;
}

async fn notation_project(fixture: &PersistedNotation) -> uuid::Uuid {
    store::notations::find_by_id(&fixture.surreal, fixture.id)
        .await
        .unwrap()
        .unwrap()
        .project_id
}

async fn review_lawyer(fixture: &PersistedNotation, name: &str) -> store::persons::Person {
    store::persons::create(
        &fixture.surreal,
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
    let Some(mut fixture) = PersistedNotation::new().await else {
        return;
    };
    let project = notation_project(&fixture).await;
    let signer = review_lawyer(&fixture, "Synthetic Signer").await;
    let mut response = fixture
        .surreal
        .query(
            "CREATE $id SET project_id = $project_id, storage_key = $storage_key, \
             content_type = 'application/pdf', byte_size = 1, sha256_hex = $sha, \
             kind = 'offboarding', visibility = 'internal', metadata = NONE",
        )
        .bind((
            "id",
            store::surreal::record_id("asset", uuid::Uuid::now_v7()),
        ))
        .bind(("project_id", store::surreal::record_id("project", project)))
        .bind(("storage_key", format!("projects/{project}/documents/test")))
        .bind(("sha", format!("sha-{project}")))
        .await
        .unwrap();
    let _: Option<serde_json::Value> = response.take(0).unwrap();
    fixture.actor = signer.id;
    fixture.start(MachineKind::Workflow,
        "BEGIN:\n  _: firm_signature__closing_letter\nfirm_signature__closing_letter:\n  signed: END\nEND: {}\n").await;
    assert_eq!(
        fixture.signal(MachineKind::Workflow, "_", None).await,
        "firm_signature__closing_letter"
    );
    assert_eq!(
        store::projects::find_by_id(&fixture.surreal, project)
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
    let closed = store::projects::find_by_id(&fixture.surreal, project)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.status, "closed");
    assert!(closed.closed_at.is_some());
    assert_eq!(fixture.events().await.len(), 2);
    assert!(fixture.email.captured().is_empty());
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn a_demoted_review_dri_falls_back_to_a_current_lawyer() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
    let project = notation_project(&fixture).await;
    let stale = review_lawyer(&fixture, "Former Reviewer").await;
    store::projects::designate_dri_in_surreal(
        &fixture.surreal,
        project,
        stale.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    store::persons::set_role(&fixture.surreal, stale.id, store::persons::Role::Client)
        .await
        .unwrap();
    let current = review_lawyer(&fixture, "Current Reviewer").await;
    store::projects::add_participation(
        &fixture.surreal,
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
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn a_signal_without_a_machine_fails_without_a_journal_or_send() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notation_machine_state WHERE notation_id = $1")
            .bind(fixture.id)
            .fetch_one(&fixture.database.pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn a_back_event_written_before_its_state_move_keeps_later_back_navigation_correct() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
        &fixture.surreal,
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
    fixture.worker.shutdown().await;
}

#[tokio::test]
async fn back_keeps_begin_and_completed_questionnaires_in_place() {
    let Some(fixture) = PersistedNotation::new().await else {
        return;
    };
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
    fixture.worker.shutdown().await;
}
