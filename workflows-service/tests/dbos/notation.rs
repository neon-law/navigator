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
