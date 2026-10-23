//! E-Filing Transaction doors: a participant records and lists a
//! transaction's status, a filed date on a pending transaction is refused, a
//! conformed copy uploaded to the Project links to an accepted row, and a
//! non-participant is refused.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use base64::Engine as _;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    project_id: Uuid,
    participant: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&surreal, "E-filing matter").await;
    let participant = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "efiling-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: participant.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role("Other Lawyer", "efiling-outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal).await;
    let app = portal::api::routes().with_state(ApiState {
        surreal: state.surreal.clone(),
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: state.storage.clone(),
        workflow_runtime: state.workflow_runtime.clone(),
        assets_storage: state.assets_storage.clone(),
        forms_registry: state.forms_registry.clone(),
        signature_provider: state.signature_provider.clone(),
        contract_reviewer: state.contract_reviewer.clone(),
        integration_providers: state.integration_providers.clone(),
    });
    Fixture {
        app,
        project_id,
        participant: portal::SessionData {
            person_id: Some(participant.id),
            ..portal::SessionData::fresh("efiling-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("efiling-outsider@example.com", Role::Lawyer)
        },
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    session: &portal::SessionData,
    body: Vec<u8>,
) -> (u16, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, json)
}

fn report(extra: &Value) -> Vec<u8> {
    let mut body = json!({
        "transaction_id": "TX-0000-1111",
        "efsp": "Example EFSP",
        "court": "Superior Court of California, County of Example",
        "case_number": "26EXCV00001"
    });
    for (key, value) in extra.as_object().unwrap() {
        body[key] = value.clone();
    }
    serde_json::to_vec(&body).unwrap()
}

async fn upload_conformed(fixture: &Fixture) -> String {
    let body = serde_json::to_vec(&json!({
        "filename": "TX-0000-1111.pdf",
        "content_base64": base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.4 synthetic"),
        "content_type": "application/pdf",
        "kind": "filing",
        "slug": "efilings/TX-0000-1111.pdf",
        "visibility": "internal"
    }))
    .unwrap();
    let (status, pointer) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/documents", fixture.project_id),
        &fixture.participant,
        body,
    )
    .await;
    assert!(matches!(status, 200 | 201), "{pointer}");
    pointer["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn a_participant_records_an_accepted_transaction_with_its_conformed_copy() {
    let fixture = fixture().await;
    let uri = format!("/app/api/projects/{}/efilings", fixture.project_id);
    let (status, submitted) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        report(&json!({"status": "submitted"})),
    )
    .await;
    assert_eq!(status, 200, "{submitted}");
    assert_eq!(submitted["status"], "submitted");
    assert_eq!(submitted["filed_on"], Value::Null);

    let conformed = upload_conformed(&fixture).await;
    let (status, accepted) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        report(&json!({
            "status": "accepted",
            "filed_on": "2026-10-05",
            "accepted_on": "2026-10-06",
            "conformed_document_id": conformed,
        })),
    )
    .await;
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["id"], submitted["id"]);
    assert_eq!(accepted["filed_on"], "2026-10-05");
    assert_eq!(accepted["accepted_on"], "2026-10-06");
    assert_eq!(accepted["conformed_document_id"], conformed);

    let (status, listed) = send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_filed_date_on_a_pending_transaction_is_refused() {
    let fixture = fixture().await;
    let (status, response) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/efilings", fixture.project_id),
        &fixture.participant,
        report(&json!({"status": "unknown", "filed_on": "2026-10-05"})),
    )
    .await;
    assert_eq!(status, 400, "{response}");
    assert_eq!(response["error"], "invalid_request");
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    let fixture = fixture().await;
    let (status, body) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/efilings", fixture.project_id),
        &fixture.outsider,
        report(&json!({"status": "submitted"})),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}
