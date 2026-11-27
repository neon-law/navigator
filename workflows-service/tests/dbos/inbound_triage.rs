use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::application_store::ApplicationStore;
use store::inbox::{ingest_inbound, IngestInbound};
use store::inbox_triage::{find_for_message, PrivilegeLabel, TriageDisposition};
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate};
use workflows::{InboundTriageReport, InboundTriageRequest};
use workflows_service::dbos_ingress::{TriageIngress, WorkerIngress};
use workflows_service::health::router_with_dbos_ingress;
use workflows_service::inbound_triage::{
    filing_workflow_id, register_dbos, register_filing_dbos, FilingWorkflow, InboundTriageService,
    TriageError, TriageProviders,
};

use crate::support::application_process::database;
use crate::support::{APP_VERSION, DEADLINE};

const ROUTE: &str = "/internal/dbos/inbound-triage";
const FILING_ROUTE: &str = "/internal/dbos/approved-inbox-filing";

async fn seed_message(
    application_store: &ApplicationStore,
    firm_id: Uuid,
    provider_message_id: &str,
) -> Uuid {
    ingest_inbound(
        application_store,
        &IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: "email",
            provider_account: "synthetic-account",
            provider_message_id,
            provider_thread_ref: None,
            external_address: "sender@example.com",
            external_name: None,
            subject: None,
            body: "Synthetic message",
            source_page: None,
            source_channel: None,
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            project_id: None,
            occurred_at: "2026-11-08T00:00:00Z",
            raw_storage_key: None,
        },
    )
    .await
    .expect("ingest a synthetic email")
    .message_id
}

#[tokio::test]
async fn inbound_triage_preserves_replay_and_confines_classifier_evidence() {
    let Some(db) = database().await else {
        return;
    };
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let (firm_id, _) =
        store::test_support::seed_firm_with_new_brand(&application_store, "neon").await;
    let message_id = seed_message(&application_store, firm_id, "triage-replay").await;
    let request = InboundTriageRequest { message_id };
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = register_dbos(
        &dbos,
        InboundTriageService::new(application_store.clone(), None),
    )
    .expect("register triage");
    let filing = register_filing_dbos(
        &dbos,
        InboundTriageService::new(application_store.clone(), None),
    )
    .expect("register filing");
    dbos.launch().await.expect("launch DBOS");
    let router = router_with_dbos_ingress(
        WorkerIngress {
            inbound_triage: Some(TriageIngress {
                token: "synthetic-service-token".into(),
                workflow: workflow.clone(),
                filing: filing.clone(),
            }),
            ..Default::default()
        },
        None,
    );
    let unauthorized = router
        .clone()
        .oneshot(Request::post(ROUTE).body(Body::empty()).unwrap())
        .await
        .expect("respond to an unauthenticated request");
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    for _ in 0..2 {
        let response = router
            .clone()
            .oneshot(
                Request::post(ROUTE)
                    .header("authorization", "Bearer synthetic-service-token")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .expect("submit triage");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    let workflow_id = format!("InboundTriage:{message_id}");
    let handle = dbos
        .retrieve_workflow::<InboundTriageReport, TriageError>(&workflow_id)
        .expect("both requests use the same Message-keyed run");
    tokio::time::timeout(DEADLINE, handle.result())
        .await
        .expect("triage completed within the deadline")
        .expect("triage succeeded");
    let original = find_for_message(&application_store, message_id)
        .await
        .expect("find the triage row")
        .expect("triage row was recorded");
    assert_eq!(original.privilege, PrivilegeLabel::Uncertain);
    assert_eq!(original.disposition, TriageDisposition::NeedsHuman);
    workflow
        .run_with(
            request,
            dbos::RunOptions {
                workflow_id: Some("synthetic-recovery"),
                ..Default::default()
            },
        )
        .await
        .expect("recover an already-recorded message");
    assert_eq!(
        find_for_message(&application_store, message_id)
            .await
            .expect("read triage after recovery"),
        Some(original)
    );
    assert_approved_filing(&router, &dbos, &filing, &application_store, firm_id).await;
    dbos.shutdown().await;

    assert_classifier_evidence_stays_in_the_inbox_audit_row(&db.url, &application_store, firm_id)
        .await;
}

async fn assert_approved_filing(
    router: &axum::Router,
    dbos: &dbos::DBOS,
    workflow: &FilingWorkflow,
    application_store: &ApplicationStore,
    firm_id: Uuid,
) {
    use store::inbox_triage::FilingApproval;

    let approval = seed_filing_approval(application_store, firm_id).await;
    let project_id = approval.project_id;
    let run = |approval| async move {
        let id = filing_workflow_id(&approval);
        workflow
            .run_with(
                approval,
                dbos::RunOptions {
                    workflow_id: Some(&id),
                    ..Default::default()
                },
            )
            .await
    };
    let refused = router
        .clone()
        .oneshot(Request::post(FILING_ROUTE).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    for _ in 0..2 {
        let submitted = router
            .clone()
            .oneshot(
                Request::post(FILING_ROUTE)
                    .header("authorization", "Bearer synthetic-service-token")
                    .body(Body::from(serde_json::to_vec(&approval).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(submitted.status(), StatusCode::ACCEPTED);
    }
    let handle = dbos
        .retrieve_workflow::<Uuid, workflows_service::inbound_triage::FilingError>(
            &filing_workflow_id(&approval),
        )
        .unwrap();
    let first = tokio::time::timeout(DEADLINE, handle.result())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run(approval).await.unwrap(), first);
    let stale = FilingApproval {
        assignment_id: Some(Uuid::now_v7()),
        ..approval
    };
    let error = run(stale).await.unwrap_err();
    assert!(matches!(
        error,
        dbos::Error::Application(workflows_service::inbound_triage::FilingError::ApprovalExpired)
    ));
    assert_eq!(
        store::communications::for_project(application_store, project_id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store::communications::for_project_client_visible(application_store, project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_attachment_filing_requires_its_exact_approval(application_store, firm_id, approval)
        .await;
}

async fn assert_attachment_filing_requires_its_exact_approval(
    db: &ApplicationStore,
    firm_id: Uuid,
    filing: store::inbox_triage::FilingApproval,
) {
    use cloud::attachment_scanner::{AttachmentScanner, ScanError, ScanVerdict};
    use store::inbox_triage::AttachmentFilingApproval;
    use workflows_service::inbox_attachment_filing::{
        file_approved_attachment, AttachmentFilingError,
    };

    struct Scanner(ScanVerdict);
    #[async_trait::async_trait]
    impl AttachmentScanner for Scanner {
        async fn scan(&self, _: &[u8]) -> Result<ScanVerdict, ScanError> {
            Ok(self.0.clone())
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let bytes = b"synthetic attachment";
    let attachment = store::inbox::ingest_attachment(
        db,
        &storage,
        firm_id,
        None,
        "fixture.txt",
        "text/plain",
        bytes,
    )
    .await
    .unwrap();
    let asset_id = attachment.asset_id;
    store::inbox::link_message_assets(db, firm_id, filing.message_id, &[attachment])
        .await
        .unwrap();
    let link = store::inbox::message_assets(db, firm_id, filing.message_id)
        .await
        .unwrap()
        .remove(0);
    let approval = AttachmentFilingApproval {
        filing,
        attachment_asset_id: asset_id,
        attachment_position: link.position,
        kind: "letter".into(),
    };
    let wrong = AttachmentFilingApproval {
        attachment_position: link.position + 10,
        ..approval.clone()
    };
    let clean = Scanner(ScanVerdict::Clean);
    assert!(matches!(
        file_approved_attachment(db, &storage, &clean, &wrong).await,
        Err(AttachmentFilingError::ApprovalExpired),
    ));
    let malicious = Scanner(ScanVerdict::Found {
        signature: "synthetic".into(),
    });
    assert!(matches!(
        file_approved_attachment(db, &storage, &malicious, &approval).await,
        Err(AttachmentFilingError::Malware),
    ));
    assert!(store::assets::for_project(db, filing.project_id)
        .await
        .unwrap()
        .is_empty());
    let first = file_approved_attachment(db, &storage, &clean, &approval)
        .await
        .unwrap();
    let repeated = file_approved_attachment(db, &storage, &clean, &approval)
        .await
        .unwrap();
    assert_eq!(first, repeated);
    let project_assets = store::assets::for_project(db, filing.project_id)
        .await
        .unwrap();
    assert_eq!(project_assets.len(), 1);
    assert_eq!(
        project_assets[0].slug.as_deref(),
        Some(format!("mail/{}/2026.11.08_fixture-0.txt", filing.message_id).as_str())
    );
    assert_eq!(
        project_assets[0].visibility,
        store::documents::visibility::INTERNAL
    );
    assert_eq!(
        project_assets[0].source_message_id.as_deref(),
        Some(filing.message_id.to_string().as_str())
    );
}

async fn seed_filing_approval(
    application_store: &ApplicationStore,
    firm_id: Uuid,
) -> store::inbox_triage::FilingApproval {
    use store::inbox_triage::{record, FilingApproval, NewInboxTriage, RULES_ONLY_PROMPT};
    use store::persons::{self, NewPerson, Role};

    let message_id = seed_message(application_store, firm_id, "triage-approved-filing").await;
    let project = store::test_support::seed_project_on_firm(
        application_store,
        Some(firm_id),
        "Synthetic Filing",
    )
    .await;
    let owner = persons::create(
        application_store,
        &NewPerson::with_role("Synthetic Reviewer", "reviewer@example.test", Role::Owner),
    )
    .await
    .unwrap();
    store::projects::add_participation(
        application_store,
        project.id,
        owner.id,
        store::projects::participation_for_role(Role::Owner),
    )
    .await
    .unwrap();
    let review = record(
        application_store,
        &NewInboxTriage {
            message_id,
            firm_id,
            suggested_project_id: Some(project.id),
            project_confidence: None,
            privilege: PrivilegeLabel::Uncertain,
            privilege_confidence: None,
            rationale: None,
            model_id: None,
            prompt_version: RULES_ONLY_PROMPT.into(),
            raw_output: None,
            disposition: TriageDisposition::NeedsHuman,
            labeled_example: false,
        },
    )
    .await
    .unwrap();
    FilingApproval {
        triage_id: review.id,
        message_id,
        project_id: project.id,
        approved_by: owner.id,
        review_revision: review.updated_at,
        assignment_id: None,
    }
}

async fn assert_classifier_evidence_stays_in_the_inbox_audit_row(
    postgres_url: &str,
    application_store: &ApplicationStore,
    firm_id: Uuid,
) {
    let message_id = seed_message(application_store, firm_id, "triage-checkpoint").await;
    let provider = MockServer::start().await;
    let rationale = "Synthetic private evidence";
    let verdict = serde_json::json!({
        "suggested_project_id": null,
        "project_confidence": 0.5,
        "privileged": true,
        "privilege_confidence": 0.9,
        "rationale": rationale,
    });
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": verdict.to_string()}]},
                "finishReason": "STOP",
            }],
        })))
        .expect(1)
        .mount(&provider)
        .await;

    let token = Arc::new(cloud::StaticTokenSource::new("synthetic-token"));
    let gemini = cloud::GeminiVertexAdapter::new(token)
        .unwrap()
        .with_base_url(provider.uri());
    let service = InboundTriageService::new(
        application_store.clone(),
        Some(TriageProviders {
            gemini,
            project_id: "synthetic-project".into(),
            model: "triage-model".into(),
            location: "global".into(),
        }),
    );
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(postgres_url, APP_VERSION));
    let workflow = register_dbos(&dbos, service).unwrap();
    dbos.launch().await.unwrap();
    let workflow_id = format!("InboundTriage:{message_id}");
    let report = workflow
        .run_with(
            InboundTriageRequest { message_id },
            dbos::RunOptions {
                workflow_id: Some(&workflow_id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(report.message_id, message_id);
    let row = find_for_message(application_store, message_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.privilege, PrivilegeLabel::Privileged);
    assert_eq!(row.rationale.as_deref(), Some(rationale));
    let steps = dbos.list_workflow_steps(&workflow_id).await.unwrap();
    assert!(!steps
        .iter()
        .filter_map(|step| step.output.as_deref())
        .any(|output| output.contains(rationale)));
    provider.verify().await;
    dbos.shutdown().await;
}
