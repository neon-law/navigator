//! Docket Entry doors (LAW-133): a participating Lawyer records, reads,
//! links, and supersedes entries; a non-participant is refused with 404; a
//! hearing without a date and a filing with one are refused.

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
    project_id: Uuid,
    participant: portal::SessionData,
    outsider: portal::SessionData,
    document_id: Uuid,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&surreal, "Docket matter").await;
    let participant = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "docket-lawyer@example.com",
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
        &NewPerson::with_role("Other Lawyer", "docket-outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal).await;
    let document_id = store::documents::ingest_bytes_as(
        &state.surreal,
        &state.assets_storage,
        &store::documents::IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename: "di-12-motion.pdf",
            kind: "filing",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("docket/9-26-cv-00042/di-12-motion.pdf"),
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
        canonical_base_url: None,
    });
    Fixture {
        app,
        project_id,
        participant: portal::SessionData {
            person_id: Some(participant.id),
            ..portal::SessionData::fresh("docket-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("docket-outsider@example.com", Role::Lawyer)
        },
        document_id,
    }
}

fn motion() -> Value {
    json!({
        "docket_number": "9:26-cv-00042",
        "caption": "Alpha v. Beta",
        "entry_number": "12",
        "kind": "motion",
        "title": "MOTION to Dismiss",
        "party": "Defendant Beta",
        "filed_or_served_on": "2026-03-04"
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
async fn a_participant_records_reads_links_and_supersedes() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/docket", f.project_id);

    // The first write opens the case from the docket number.
    let (status, filed) = send(&f.app, "POST", &uri, &f.participant, motion()).await;
    assert_eq!(status, 201, "{filed}");
    assert_eq!(filed["entry_number"], "12");
    assert!(filed["document_asset_id"].is_null(), "source pending");
    let filed_id = filed["id"].as_str().unwrap().to_string();

    // The same entry number again reads the row back instead of duplicating.
    let (status, again) = send(&f.app, "POST", &uri, &f.participant, motion()).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["id"], filed["id"]);

    let (status, linked) = send(
        &f.app,
        "POST",
        &format!("{uri}/{filed_id}/link-document"),
        &f.participant,
        json!({ "slug": "docket/9-26-cv-00042/di-12-motion.pdf" }),
    )
    .await;
    assert_eq!(status, 200, "{linked}");
    assert_eq!(linked["document_asset_id"], f.document_id.to_string());

    let (status, hearing) = send(
        &f.app,
        "POST",
        &uri,
        &f.participant,
        json!({
            "entry_number": "13",
            "kind": "hearing",
            "title": "Hearing on motion to dismiss",
            "scheduled_on": "2027-03-15T09:00:00-07:00"
        }),
    )
    .await;
    assert_eq!(status, 201, "{hearing}");
    let hearing_id = hearing["id"].as_str().unwrap().to_string();

    let (status, continued) = send(
        &f.app,
        "POST",
        &format!("{uri}/{hearing_id}/supersede"),
        &f.participant,
        json!({
            "entry_number": "14",
            "scheduled_on": "2027-04-01T09:00:00-07:00",
            "document_id": f.document_id
        }),
    )
    .await;
    assert_eq!(status, 201, "{continued}");
    assert_eq!(continued["supersedes"], hearing_id);
    assert_eq!(continued["kind"], "hearing");

    let (status, docket) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(status, 200, "{docket}");
    assert_eq!(docket["cases"].as_array().unwrap().len(), 1);
    assert_eq!(docket["cases"][0]["docket_number"], "9:26-cv-00042");
    assert_eq!(docket["cases"][0]["caption"], "Alpha v. Beta");
    let numbers: Vec<&str> = docket["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["entry_number"].as_str().unwrap())
        .collect();
    assert_eq!(numbers, ["12", "13", "14"]);
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/docket", f.project_id);
    let (status, body) = send(&f.app, "POST", &uri, &f.outsider, motion()).await;
    assert_eq!(status, 404, "{body}");
    let (status, body) = send(&f.app, "GET", &uri, &f.outsider, Value::Null).await;
    assert_eq!(status, 404, "{body}");
    let (status, body) = send(
        &f.app,
        "POST",
        &format!("{uri}/{}/link-document", Uuid::now_v7()),
        &f.outsider,
        json!({ "document_id": f.document_id }),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn an_undated_hearing_a_dated_filing_and_an_unknown_kind_are_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/docket", f.project_id);
    let mut hearing = motion();
    hearing["kind"] = json!("hearing");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, hearing).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("scheduled_on"));

    let mut dated = motion();
    dated["scheduled_on"] = json!("2027-03-15T09:00:00-07:00");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, dated).await;
    assert_eq!(status, 400, "{body}");

    let mut unknown = motion();
    unknown["kind"] = json!("memo");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, unknown).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("motion"));

    // A document from nowhere is not this Project's.
    let mut foreign = motion();
    foreign["document_id"] = json!(Uuid::now_v7());
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, foreign).await;
    assert_eq!(status, 400, "{body}");

    // No case and no docket number: the write cannot pick a case.
    let (status, body) = send(
        &f.app,
        "POST",
        &uri,
        &f.participant,
        json!({ "entry_number": "1", "kind": "other", "title": "Entry" }),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"].as_str().unwrap().contains("docket_number"));
}
