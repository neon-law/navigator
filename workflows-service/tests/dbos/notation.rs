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
    _database: crate::support::TestDatabase,
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
            _database: database,
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
