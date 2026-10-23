//! Service doors (LAW-124): a participating Lawyer records, lists, and reads
//! one back; a non-participant is refused with 404; an unknown method and a
//! non-date are refused; the reference names the docket entry where one
//! links the document and the slug otherwise.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    participant: portal::SessionData,
    outsider: portal::SessionData,
    document_id: Uuid,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&surreal, "Service matter").await;
    let participant = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "service-lawyer@example.com",
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
        &NewPerson::with_role("Other Lawyer", "service-outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal.clone()).await;
    let document_id = store::documents::ingest_bytes_as(
        &state.surreal,
        &state.assets_storage,
        &store::documents::IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename: "motion.pdf",
            kind: "pleading",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("pleadings/motion.pdf"),
            ..Default::default()
        },
        b"%PDF-1.7 synthetic",
    )
    .await
    .unwrap()
    .asset_id;
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
        surreal,
        project_id,
        participant: portal::SessionData {
            person_id: Some(participant.id),
            ..portal::SessionData::fresh("service-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("service-outsider@example.com", Role::Lawyer)
        },
        document_id,
    }
}

fn by_mail() -> Value {
    json!({
        "slug": "pleadings/motion.pdf",
        "recipient": "Example Counsel",
        "counsel_for": "Defendant Example Co.",
        "method": "mail",
        "served_on": "2026-03-05",
        "address": "200 Sample Street, Sampleton, CA 90000",
        "declarant": "Pat Sample"
    })
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    session: &portal::SessionData,
    body: Value,
) -> (u16, Value) {
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).unwrap()
    };
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(bytes))
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

#[tokio::test]
async fn a_participant_records_lists_and_shows_with_a_document_reference() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/service", f.project_id);
    let (status, created) = send(&f.app, "POST", &uri, &f.participant, by_mail()).await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["method"], "mail");
    assert_eq!(created["document_id"], f.document_id.to_string());
    assert_eq!(
        created["reference"], "pleadings/motion.pdf",
        "no docket entry yet: the slug"
    );
    let id = created["id"].as_str().unwrap().to_string();

    // Once a docket entry links the document, the reference is the entry.
    let case = store::cases::open_case(
        &f.surreal,
        &store::cases::NewCase {
            project_id: f.project_id,
            caption: "Alpha v. Beta",
            forum: None,
            jurisdiction: None,
            docket_number: Some("9:26-cv-00042"),
            judge: None,
            posture: "plaintiff",
        },
    )
    .await
    .unwrap();
    store::cases::record_entry(
        &f.surreal,
        &store::cases::NewDocketEntry {
            case_id: case.id,
            entry_number: "12",
            kind: store::cases::EntryKind::Motion,
            title: "MOTION",
            party: None,
            filed_or_served_on: None,
            scheduled_on: None,
            supersedes: None,
            document_asset_id: Some(f.document_id),
            notation_id: None,
        },
    )
    .await
    .unwrap();

    let mut by_email = by_mail();
    by_email["document_id"] = json!(f.document_id);
    by_email.as_object_mut().unwrap().remove("slug");
    by_email["recipient"] = json!("Other Counsel");
    by_email["method"] = json!("e_service");
    by_email["address"] = json!("other@example.com");
    let (status, second) = send(&f.app, "POST", &uri, &f.participant, by_email).await;
    assert_eq!(status, 201, "{second}");
    assert_eq!(second["reference"], "D.I. 12");

    let (status, listed) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 2);
    assert_eq!(listed[0]["id"], id);
    assert_eq!(listed[0]["reference"], "D.I. 12");

    let (status, shown) = send(
        &f.app,
        "GET",
        &format!("{uri}/{id}"),
        &f.participant,
        Value::Null,
    )
    .await;
    assert_eq!(status, 200, "{shown}");
    assert_eq!(shown["recipient"], "Example Counsel");
    let (status, _) = send(
        &f.app,
        "GET",
        &format!("{uri}/{}", Uuid::now_v7()),
        &f.participant,
        Value::Null,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/service", f.project_id);
    let (status, body) = send(&f.app, "POST", &uri, &f.outsider, by_mail()).await;
    assert_eq!(status, 404, "{body}");
    let (status, body) = send(&f.app, "GET", &uri, &f.outsider, Value::Null).await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn an_unknown_method_a_non_date_and_an_unnamed_document_are_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/service", f.project_id);
    let mut unknown = by_mail();
    unknown["method"] = json!("fax");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, unknown).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"], "invalid_request");
    assert!(body["message"].as_str().unwrap().contains("certified_mail"));

    let mut non_date = by_mail();
    non_date["served_on"] = json!("March 5, 2026");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, non_date).await;
    assert_eq!(status, 400, "{body}");

    let mut unnamed = by_mail();
    unnamed.as_object_mut().unwrap().remove("slug");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, unnamed).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("document_id"));

    let mut foreign = by_mail();
    foreign["slug"] = json!("pleadings/elsewhere.pdf");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, foreign).await;
    assert_eq!(status, 400, "{body}");
}
