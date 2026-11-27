//! Committed native Notations are visible to the real DBOS contract-review worker.
use futures::FutureExt as _;
use portal::contract_review_walk::{drive_contract_review, ReviewDeps};
use sqlx::ConnectOptions as _;
use std::sync::Arc;
use store::playbooks::{NewPlaybook, Position};
use uuid::Uuid;
use workflows::{DbosNotationRuntime, IntakeArtifact, StateMachineRuntime};
use workflows_service::dbos_ingress::{NotationIngress, WorkerIngress};
#[path = "../../store/tests/support/native_project.rs"]
mod native_project;

#[tokio::test]
async fn contract_review_hands_committed_notation_to_native_dbos() {
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = pg.pool().connect_options().to_url_lossy().to_string();
        let worker = dbos::DBOS::new(workflows::dbos_engine::config(&url, "contract-review-test"));
        let directory = tempfile::tempdir().unwrap();
        let storage: Arc<dyn cloud::StorageService> =
            Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
        let workflow = workflows_service::dbos_notation::register(
            &worker,
            workflows_service::dbos_notation::Context::new(
                store::application_store::ApplicationStore::uninitialized(),
                pg.clone(),
                Arc::new(workflows::CapturingEmail::default()),
                storage.clone(),
            ),
        )
        .unwrap();
        let router = workflows_service::health::router_with_dbos_ingress(
            WorkerIngress {
                notation: Some(NotationIngress::new(
                    "synthetic-contract-review-token".into(),
                    worker.clone(),
                    pg.clone(),
                    workflow,
                )),
                ..Default::default()
            },
            None,
        );
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        let runtime = DbosNotationRuntime::new(
            format!("http://{address}"),
            Some("synthetic-contract-review-token".into()),
        );
        let result = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(std::time::Duration::from_secs(60), async {
                worker.launch().await.unwrap();
                review(&runtime, &storage).await;
            })
            .await
            .expect("native contract-review handoff must finish within the test bound");
        })
        .catch_unwind()
        .await;
        worker.shutdown().await;
        let _ = stop.send(());
        server.await.unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    })
    .await;
}

async fn seed_matter(db: &store::application_store::ApplicationStore) -> (Uuid, Uuid, Uuid) {
    let unique = Uuid::now_v7();
    let project = native_project::create(&format!("contract-review-native-{unique}")).await;
    let person = store::persons::create(
        db,
        &store::persons::NewPerson::new(
            "Synthetic client",
            format!("client-{unique}@example.test"),
        ),
    )
    .await
    .unwrap();
    (project.id, person.id, project.entity_id)
}

fn sample_positions() -> Vec<Position> {
    vec![
        Position {
            topic: "Limitation of liability".into(),
            preferred: "Mutual cap at 12 months' fees".into(),
            fallback: "Cap at 2x fees paid".into(),
            walkaway: "Uncapped liability".into(),
            severity: store::playbooks::SEVERITY_HIGH.into(),
        },
        Position {
            topic: "Governing law".into(),
            preferred: "Nevada".into(),
            fallback: "Delaware".into(),
            walkaway: "A jurisdiction with no nexus".into(),
            severity: store::playbooks::SEVERITY_MEDIUM.into(),
        },
    ]
}

async fn review(runtime: &DbosNotationRuntime, storage: &Arc<dyn cloud::StorageService>) {
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let contract_reviewer = portal::contract_review::StubContractReviewer;
    let (project_id, person_id, entity_id) = seed_matter(&application_store).await;
    store::templates::save_version(
        &application_store,
        None,
        "memo__contract_review",
        store::templates::Version {
            title: "Inbound Contract Review".into(),
            respondent_type: "person_and_entity".into(),
            asset_id: None,
            form_code: None,
            kind: None,
            source_commit_sha: None,
        },
    )
    .await
    .expect("seed the contract-review template");
    no_playbook(
        &ReviewDeps {
            application_store: &application_store,
            workflow_runtime: runtime,
            storage,
            contract_reviewer: &contract_reviewer,
        },
        project_id,
        person_id,
    )
    .await;
    let scoped_template = store::test_support::seed_unpinned_project_template(
        &application_store,
        project_id,
        "memo__contract_review",
        "Project Contract Review",
        "person_and_entity",
        None,
    )
    .await;
    let positions = sample_positions();
    let playbook_id = store::playbooks::create(
        &application_store,
        &NewPlaybook {
            entity_id,
            name: "Synthetic contract-review playbook",
            positions: &positions,
        },
    )
    .await
    .expect("create playbook");

    let deps = ReviewDeps {
        application_store: &application_store,
        workflow_runtime: runtime,
        storage,
        contract_reviewer: &contract_reviewer,
    };

    let review_id = drive_contract_review(
        &deps,
        project_id,
        person_id,
        "vendor-msa.txt",
        "MASTER SERVICES AGREEMENT\nLiability is uncapped. Governed by the laws of Mars.",
        IntakeArtifact::Text {
            text: "MASTER SERVICES AGREEMENT\nLiability is uncapped.".into(),
        },
    )
    .await
    .expect("the native worker must read the committed Notation and complete contract intake");

    let notation_id = assert_review(
        &application_store,
        review_id,
        project_id,
        entity_id,
        playbook_id,
        &scoped_template,
    )
    .await;
    assert!(
        !runtime
            .events(workflows::MachineKind::Workflow, notation_id)
            .await
            .is_empty(),
        "the authenticated native ingress must return persisted workflow events"
    );
}

async fn no_playbook(deps: &ReviewDeps<'_>, project_id: Uuid, person_id: Uuid) {
    let err = portal::contract_review_walk::drive_contract_review(
        deps,
        project_id,
        person_id,
        "vendor-msa.txt",
        "contract body",
        IntakeArtifact::Text {
            text: "contract body".into(),
        },
    )
    .await
    .expect_err("no playbook on file");
    assert!(matches!(
        err,
        portal::contract_review_walk::ContractReviewError::NoPlaybook
    ));

    // No notation was opened — we fail before touching the workflow.
    let count = store::notations::list_by_project(deps.application_store, project_id)
        .await
        .unwrap()
        .len();
    assert_eq!(count, 0);
}

async fn assert_review(
    db: &store::application_store::ApplicationStore,
    review_id: Uuid,
    project_id: Uuid,
    entity_id: Uuid,
    playbook_id: Uuid,
    scoped_template: &store::templates::Template,
) -> Uuid {
    // The review row carries the playbook, a risk summary, and one finding
    // per playbook position — every one un-accepted (the attorney must act).
    let review = store::contract_reviews::by_id(db, review_id)
        .await
        .unwrap()
        .expect("review row exists");
    assert_eq!(review.playbook_id, playbook_id);
    assert_eq!(review.status, store::contract_reviews::STATUS_ANALYZED);
    let findings = store::contract_reviews::findings_of(&review).unwrap();
    assert_eq!(findings.len(), 2, "one finding per playbook position");
    assert!(findings.iter().all(|f| !f.accepted));
    assert!(review.risk_summary.is_some());

    // The inbound contract was filed into the project as an assets row.
    assert!(
        review.asset_id.is_some(),
        "the filed inbound-contract document is linked"
    );

    // The notation reached the attorney gate.
    let notation_row = store::notations::list_by_project(db, project_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("notation exists");
    assert_eq!(notation_row.state, "lawyer_review");
    assert_eq!(notation_row.entity_id, Some(entity_id));
    assert_eq!(
        notation_row.template_id, scoped_template.id,
        "contract review must pin the current project-scoped template"
    );
    notation_row.id
}
