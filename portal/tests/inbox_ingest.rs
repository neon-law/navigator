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
    firm_id: uuid::Uuid,
    lawyer_id: uuid::Uuid,
    session: portal::SessionData,
    surreal: store::surreal::SurrealDb,
}

/// Seed a Project owned by `firm_id` on which `lawyer_id` is the lawyer DRI.
async fn seed_firm_project(
    db: &store::surreal::SurrealDb,
    code: &str,
    firm_id: uuid::Uuid,
    lawyer_id: uuid::Uuid,
) -> uuid::Uuid {
    let project_id = store::test_support::seed_project_surreal(db, code).await;
    store::projects::update_project(
        db,
        project_id,
        &store::projects::UpdateProjectCommand {
            firm_id: Some(firm_id),
            brand: Some("neon".into()),
            ..Default::default()
        },
    )
    .await
    .expect("attach the owning Firm");
    store::projects::designate_dri_in_surreal(
        db,
        project_id,
        lawyer_id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .expect("seed the synthetic lawyer's participation row");
    project_id
}

async fn fixture(code: &str) -> Fixture {
    let db = mem_surreal().await;
    let firm_id = store::test_support::seed_firm_with_brand(&db, "neon").await;

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
    let project_id = seed_firm_project(&db, code, firm_id, lawyer.id).await;

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
        billing_provider: state.billing_provider.clone(),
        canonical_base_url: None,
    };
    let session = portal::SessionData {
        person_id: Some(lawyer.id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };

    Fixture {
        app: portal::api::routes().with_state(api_state),
        project_id,
        firm_id,
        lawyer_id: lawyer.id,
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
    post_ingest_to(fixture, fixture.project_id, session, raw_field).await
}

async fn post_ingest_to(
    fixture: &Fixture,
    project_id: uuid::Uuid,
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
                .uri(format!("/app/api/projects/{project_id}/inbox-ingest"))
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

/// A synthetic message from `from` whose thread headers are `thread_headers`
/// (each a full `Name: value` line, or none).
fn threaded_eml(from: &str, message_id: &str, thread_headers: &[&str]) -> String {
    let mut headers = String::new();
    for line in thread_headers {
        headers.push_str(line);
        headers.push_str("\r\n");
    }
    format!(
        "From: {from}\r\n\
         To: client@example.com\r\n\
         Subject: Re: Synthetic ingest\r\n\
         Message-ID: <{message_id}>\r\n\
         {headers}\
         Date: Thu, 29 Oct 2026 13:00:00 +0000\r\n\
         \r\n\
         Synthetic reply.\r\n"
    )
}

async fn ingest_raw(fixture: &Fixture, project_id: uuid::Uuid, raw: &str) -> Value {
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
    let (status, body) =
        post_ingest_to(fixture, project_id, &fixture.session, json!(encoded)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

#[tokio::test]
async fn a_reply_naming_an_ingested_message_in_in_reply_to_joins_its_conversation() {
    let fixture = fixture("inbox-ingest-reply").await;
    let original = ingest_raw(
        &fixture,
        fixture.project_id,
        &synthetic_eml("thread-root@example.com"),
    )
    .await;
    let reply = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Firm <lawyer@firm.example>",
            "thread-reply@firm.example",
            &[
                "In-Reply-To: <thread-root@example.com>",
                "References: <thread-root@example.com>",
            ],
        ),
    )
    .await;
    assert_eq!(reply["deduped"], json!(false));
    assert_ne!(reply["message_id"], original["message_id"]);
    assert_eq!(reply["conversation_id"], original["conversation_id"]);

    // Re-ingesting the reply still dedupes onto the same Message.
    let again = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Firm <lawyer@firm.example>",
            "thread-reply@firm.example",
            &["In-Reply-To: <thread-root@example.com>"],
        ),
    )
    .await;
    assert_eq!(again["deduped"], json!(true));
    assert_eq!(again["message_id"], reply["message_id"]);
    assert_eq!(again["conversation_id"], original["conversation_id"]);
}

#[tokio::test]
async fn the_references_chain_threads_a_message_without_in_reply_to() {
    let fixture = fixture("inbox-ingest-references").await;
    let original = ingest_raw(
        &fixture,
        fixture.project_id,
        &synthetic_eml("chain-root@example.com"),
    )
    .await;
    let later = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Co-counsel <cocounsel@other.example>",
            "chain-3@other.example",
            &["References: <chain-root@example.com> <chain-2-never-ingested@firm.example>"],
        ),
    )
    .await;
    assert_eq!(later["deduped"], json!(false));
    assert_eq!(later["conversation_id"], original["conversation_id"]);
}

#[tokio::test]
async fn an_unrelated_message_starts_a_new_conversation() {
    let fixture = fixture("inbox-ingest-unrelated").await;
    let original = ingest_raw(
        &fixture,
        fixture.project_id,
        &synthetic_eml("unrelated-root@example.com"),
    )
    .await;
    let unrelated = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Stranger <stranger@elsewhere.example>",
            "unrelated-2@elsewhere.example",
            &["In-Reply-To: <some-other-thread@elsewhere.example>"],
        ),
    )
    .await;
    assert_eq!(unrelated["deduped"], json!(false));
    assert_ne!(unrelated["conversation_id"], original["conversation_id"]);
}

#[tokio::test]
async fn a_reply_to_a_message_filed_in_another_project_does_not_join_it() {
    let fixture = fixture("inbox-ingest-scope-a").await;
    let other_project = seed_firm_project(
        &fixture.surreal,
        "inbox-ingest-scope-b",
        fixture.firm_id,
        fixture.lawyer_id,
    )
    .await;
    let elsewhere = ingest_raw(
        &fixture,
        other_project,
        &synthetic_eml("scope-root@example.com"),
    )
    .await;
    let reply = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Firm <lawyer@firm.example>",
            "scope-reply@firm.example",
            &["In-Reply-To: <scope-root@example.com>"],
        ),
    )
    .await;
    assert_eq!(reply["deduped"], json!(false));
    assert_ne!(reply["conversation_id"], elsewhere["conversation_id"]);
}

#[tokio::test]
async fn a_shared_gmail_thread_id_joins_one_conversation() {
    let fixture = fixture("inbox-ingest-gmail-thread").await;
    let first = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Client <client@example.com>",
            "gm-1@example.com",
            &["X-GM-THRID: 1790000000000000001"],
        ),
    )
    .await;
    let second = ingest_raw(
        &fixture,
        fixture.project_id,
        &threaded_eml(
            "Firm <lawyer@firm.example>",
            "gm-2@firm.example",
            &["X-GM-THRID: 1790000000000000001"],
        ),
    )
    .await;
    assert_eq!(second["conversation_id"], first["conversation_id"]);
}
