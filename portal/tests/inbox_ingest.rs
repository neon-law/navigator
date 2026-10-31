//! In-process route tests for the CLI message-ingest door
//! (`POST /app/api/projects/{id}/inbox-ingest`). No external matter data is
//! used; every fixture is synthetic.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use base64::Engine as _;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;

struct Fixture {
    app: Router,
    project_id: uuid::Uuid,
    session: portal::SessionData,
    surreal: store::surreal::SurrealDb,
}

async fn fixture(code: &str) -> Fixture {
    let db = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&db, code).await;
    let firm_id = store::test_support::seed_firm_with_brand(&db, "neon").await;
    store::projects::update_project(
        &db,
        project_id,
        &store::projects::UpdateProjectCommand {
            firm_id: Some(firm_id),
            brand: Some("neon".into()),
            ..Default::default()
        },
    )
    .await
    .expect("attach the owning Firm");

    let lawyer = store::test_support::ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: "synthetic-lawyer@example.com".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    store::projects::designate_dri_in_surreal(
        &db,
        project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .expect("seed the synthetic lawyer's participation row");

    let state = portal::test_support::app_state(db).await;
    let api_state = ApiState {
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
    };
    let session = portal::SessionData {
        person_id: Some(lawyer.id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };

    Fixture {
        app: portal::api::routes().with_state(api_state),
        project_id,
        session,
        surreal: state.surreal.clone(),
    }
}

/// A minimal but fully parseable RFC 5322 message.
fn synthetic_eml(message_id: &str) -> String {
    format!(
        "From: Client <client@example.com>\r\n\
         To: nick@neonlaw.com\r\n\
         Subject: Synthetic ingest\r\n\
         Message-ID: <{message_id}>\r\n\
         Date: Thu, 29 Oct 2026 12:00:00 +0000\r\n\
         \r\n\
         Synthetic body.\r\n"
    )
}

async fn post_ingest(
    fixture: &Fixture,
    session: &portal::SessionData,
    raw_field: Value,
) -> (StatusCode, Value) {
    let body = json!({
        "raw": raw_field,
        "provider_account": "ingest-test@example.com",
    });
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/app/api/projects/{}/inbox-ingest",
                    fixture.project_id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn ingest_files_a_raw_eml_into_the_project_inbox() {
    let fixture = fixture("inbox-ingest-happy").await;
    let raw = synthetic_eml("ingest-happy-1@example.com");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
    let (status, body) = post_ingest(&fixture, &fixture.session, json!(encoded)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["deduped"], json!(false));
    assert!(body["conversation_id"].as_str().is_some());
    assert!(body["message_id"].as_str().is_some());
}

#[tokio::test]
async fn redelivery_of_the_same_message_dedupes() {
    let fixture = fixture("inbox-ingest-dedup").await;
    let raw = synthetic_eml("ingest-dedup-1@example.com");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());

    let (first_status, first) =
        post_ingest(&fixture, &fixture.session, json!(encoded.clone())).await;
    assert_eq!(first_status, StatusCode::CREATED);
    assert_eq!(first["deduped"], json!(false));

    let (second_status, second) = post_ingest(&fixture, &fixture.session, json!(encoded)).await;
    assert_eq!(second_status, StatusCode::CREATED);
    assert_eq!(second["deduped"], json!(true));
    assert_eq!(first["message_id"], second["message_id"]);
}

#[tokio::test]
async fn a_lawyer_outside_the_project_gets_not_found() {
    let mut fixture = fixture("inbox-ingest-outside").await;
    let db = fixture.surreal.clone();
    let outsider = store::test_support::ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Outsider".into(),
            email: "synthetic-outsider@example.com".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    fixture.session = portal::SessionData {
        person_id: Some(outsider.id),
        ..portal::SessionData::fresh("synthetic-outsider-sub", Role::Lawyer)
    };

    let raw = synthetic_eml("ingest-outside-1@example.com");
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
    let (status, _) = post_ingest(&fixture, &fixture.session, json!(encoded)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_base64_is_rejected() {
    let fixture = fixture("inbox-ingest-bad64").await;
    let (status, body) = post_ingest(&fixture, &fixture.session, json!("not base64!!")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], json!("invalid_base64"));
}
