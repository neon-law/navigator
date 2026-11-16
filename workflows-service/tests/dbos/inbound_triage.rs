use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use store::inbox::{ingest_inbound, IngestInbound};
use store::inbox_triage::{find_for_message, PrivilegeLabel, TriageDisposition};
use store::surreal::SurrealDb;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate};
use workflows::{InboundTriageReport, InboundTriageRequest};
use workflows_service::dbos_ingress::{TriageIngress, WorkerIngress};
use workflows_service::health::router_with_dbos_ingress;
use workflows_service::inbound_triage::{
    register_dbos, InboundTriageService, TriageError, TriageProviders,
};

use crate::support::{database, APP_VERSION, DEADLINE};

const ROUTE: &str = "/internal/dbos/inbound-triage";

async fn seed_message(surreal: &SurrealDb) -> Uuid {
    let firm_id = store::test_support::seed_firm_with_brand(surreal, "neon").await;
    ingest_inbound(
        surreal,
        &IngestInbound {
            firm_id,
            brand_key: Some("neon"),
            interaction_type: None,
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: "email",
            provider_account: "synthetic-account",
            provider_message_id: "triage-replay",
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
async fn a_replayed_inbound_message_keeps_one_rules_only_triage_row() {
    let Some(db) = database().await else {
        return;
    };
    let surreal = store::test_support::mem_surreal().await;
    let message_id = seed_message(&surreal).await;
    let request = InboundTriageRequest { message_id };
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
    let workflow = register_dbos(&dbos, InboundTriageService::new(surreal.clone(), None))
        .expect("register triage");
    dbos.launch().await.expect("launch DBOS");
    let router = router_with_dbos_ingress(
        WorkerIngress {
            inbound_triage: Some(TriageIngress {
                token: "synthetic-service-token".into(),
                workflow: workflow.clone(),
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
    let original = find_for_message(&surreal, message_id)
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
        find_for_message(&surreal, message_id)
            .await
            .expect("read triage after recovery"),
        Some(original)
    );
    dbos.shutdown().await;
}

#[tokio::test]
async fn classifier_evidence_stays_in_the_inbox_audit_row() {
    let Some(db) = database().await else {
        return;
    };
    let surreal = store::test_support::mem_surreal().await;
    let message_id = seed_message(&surreal).await;
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
        surreal.clone(),
        Some(TriageProviders {
            gemini,
            project_id: "synthetic-project".into(),
            model: "triage-model".into(),
            location: "global".into(),
        }),
    );
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&db.url, APP_VERSION));
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
    let row = find_for_message(&surreal, message_id)
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
