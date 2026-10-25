//! Project deadline doors: a participant can create, list, update, and
//! complete; a non-participant is refused; a statute without a citation is
//! refused.

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
    other_project_id: Uuid,
    participant: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&surreal, "Deadline matter").await;
    let participant = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "deadline-lawyer@example.com",
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
    let other_project_id =
        store::test_support::seed_project_surreal(&surreal, "Other synthetic deadline matter")
            .await;
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id: other_project_id,
            person_id: participant.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Other Lawyer",
            "deadline-outsider@example.com",
            Role::Lawyer,
        ),
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
        other_project_id,
        participant: portal::SessionData {
            person_id: Some(participant.id),
            ..portal::SessionData::fresh("deadline-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("deadline-outsider@example.com", Role::Lawyer)
        },
    }
}

fn body(authority: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "due_at": "2026-11-02T17:00:00-08:00",
        "authority_kind": "statute",
        "authority": authority,
        "calendar_role": "firm",
        "internal_lead_days": 14,
        "client_lead_days": 7
    }))
    .unwrap()
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

#[tokio::test]
async fn a_participant_creates_lists_updates_and_completes() {
    let fixture = fixture().await;
    let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
    let (status, created) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        body("15 U.S.C. § 1"),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["status"], "open");
    assert_eq!(created["calendar_role"], "firm");
    let id = created["id"].as_str().unwrap();

    let (status, listed) = send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let (status, updated) = send(
        &fixture.app,
        "PATCH",
        &format!("{uri}/{id}"),
        &fixture.participant,
        serde_json::to_vec(&json!({"calendar_role": "opponent"})).unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(updated["calendar_role"], "opponent");
    assert_eq!(updated["status"], "open");

    let (status, completed) = send(
        &fixture.app,
        "POST",
        &format!("{uri}/{id}/complete"),
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 200, "{completed}");
    assert_eq!(completed["status"], "complete");
    assert_eq!(completed["id"], id);
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    let fixture = fixture().await;
    let (status, body) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/deadlines", fixture.project_id),
        &fixture.outsider,
        body("15 U.S.C. § 1"),
    )
    .await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn a_statute_without_a_citation_is_refused() {
    let fixture = fixture().await;
    let (status, response) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/deadlines", fixture.project_id),
        &fixture.participant,
        body("  "),
    )
    .await;
    assert_eq!(status, 400, "{response}");
    assert_eq!(response["error"], "invalid_request");
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn cancelled_replay_requires_explicit_reopen_on_the_same_project() {
    let fixture = fixture().await;
    let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
    let mut input: Value = serde_json::from_slice(&body("Synthetic order")).unwrap();
    input["replay_key"] = json!("notice-1");
    let (status, created) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        serde_json::to_vec(&input).unwrap(),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_str().unwrap();
    let (status, cancelled) = send(
        &fixture.app,
        "POST",
        &format!("{uri}/{id}/cancel"),
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 200, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    input["due_at"] = json!("2026-12-15T12:00:00Z");
    let (status, refused) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        serde_json::to_vec(&input).unwrap(),
    )
    .await;
    assert_eq!(status, 400, "{refused}");
    assert!(refused.to_string().contains("cancelled"));
    assert!(refused.to_string().contains("reopen"));
    let (_, listed) = send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
    assert_eq!(listed, json!([cancelled]));
    let reopen = format!("{uri}/{id}/reopen");
    let (status, _) = send(&fixture.app, "POST", &reopen, &fixture.outsider, Vec::new()).await;
    assert_eq!(status, 404);
    let foreign = format!(
        "/app/api/projects/{}/deadlines/{id}/reopen",
        fixture.other_project_id
    );
    let (status, _) = send(
        &fixture.app,
        "POST",
        &foreign,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 404);
    let (status, reopened) = send(
        &fixture.app,
        "POST",
        &reopen,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 200, "{reopened}");
    let mut expected = created.clone();
    expected["updated_at"] = reopened["updated_at"].clone();
    assert_eq!(reopened, expected);
    let (status, repeated) = send(
        &fixture.app,
        "POST",
        &reopen,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 200, "{repeated}");
    assert_eq!(repeated, reopened);
    let (status, replayed) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        serde_json::to_vec(&input).unwrap(),
    )
    .await;
    assert_eq!(status, 201, "{replayed}");
    assert_eq!(replayed["id"], id);
    assert_eq!(replayed["status"], "open");
    assert_eq!(replayed["due_at"], "2026-12-15T12:00:00+00:00");
    let (status, completed) = send(
        &fixture.app,
        "POST",
        &format!("{uri}/{id}/complete"),
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 200, "{completed}");
    let (status, refused) = send(
        &fixture.app,
        "POST",
        &reopen,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(status, 400, "{refused}");
    assert!(refused.to_string().contains("complete"));
    let (_, listed) = send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
    assert_eq!(listed, json!([completed]));
}

#[tokio::test]
async fn a_title_is_stored_changed_and_cleared() {
    let fixture = fixture().await;
    let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
    let mut input: Value = serde_json::from_slice(&body("15 U.S.C. § 1")).unwrap();
    input["title"] = json!("Answer to the complaint");
    let (status, created) = send(
        &fixture.app,
        "POST",
        &uri,
        &fixture.participant,
        serde_json::to_vec(&input).unwrap(),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["title"], "Answer to the complaint");
    let id = created["id"].as_str().unwrap();

    let (status, renamed) = send(
        &fixture.app,
        "PATCH",
        &format!("{uri}/{id}"),
        &fixture.participant,
        serde_json::to_vec(&json!({"title": "Reply brief"})).unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(renamed["title"], "Reply brief");

    let (status, cleared) = send(
        &fixture.app,
        "PATCH",
        &format!("{uri}/{id}"),
        &fixture.participant,
        serde_json::to_vec(&json!({"title": ""})).unwrap(),
    )
    .await;
    assert_eq!(status, 200, "{cleared}");
    assert!(cleared["title"].is_null(), "{cleared}");
}
