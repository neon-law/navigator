//! Project deadline doors: a participant can create, list, update, and
//! complete; a non-participant is refused; a statute without a citation is
//! refused.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use store::test_support::with_postgres;
use tower::ServiceExt;
use uuid::Uuid;

#[path = "deadlines_api/forgejo.rs"]
mod forgejo;

struct Fixture {
    app: Router,
    project_id: Uuid,
    other_project_id: Uuid,
    participant: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    fixture_with_integrations(None).await
}
async fn fixture_with_integrations(mock: Option<&wiremock::MockServer>) -> Fixture {
    let application_store = store::application_store::ApplicationStore::uninitialized();
    let firm = if mock.is_some() {
        store::test_support::seed_compiled_brands_postgres(&application_store).await;
        store::brands::find_by_key(&application_store, "neon")
            .await
            .unwrap()
            .unwrap()
            .firm_id
    } else {
        None
    };
    let project_id =
        store::test_support::seed_project_on_firm(&application_store, firm, "Deadline matter")
            .await
            .id;
    let participant = store::persons::create(
        &application_store,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "deadline-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let other_project_id = store::test_support::seed_project_on_firm(
        &application_store,
        None,
        "Other synthetic deadline matter",
    )
    .await
    .id;
    for id in [project_id, other_project_id] {
        store::participation::add_participant(
            &application_store,
            &store::participation::AddParticipantCommand {
                project_id: id,
                person_id: participant.id,
                dri: store::participation::DriRequest::Unchanged,
                actor: store::participation::DriActor::System,
            },
        )
        .await
        .unwrap();
    }
    let outsider = store::persons::create(
        &application_store,
        &NewPerson::with_role(
            "Other Lawyer",
            "deadline-outsider@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(application_store).await;
    let mut providers = state.integration_providers.clone();
    if let Some(mock) = mock {
        let firm = store::firms::find_by_id(&state.application_store, firm.unwrap())
            .await
            .unwrap()
            .unwrap();
        let mut fake = portal::integrations::FakeIntegrations::new();
        fake.forgejo =
            Some(cloud::ForgejoForge::new(&mock.uri(), &firm.slug, "fixture-secret").unwrap());
        providers = std::sync::Arc::new(fake);
    }
    let app = portal::api::routes().with_state(ApiState {
        jev: None,
        application_store: state.application_store.clone(),
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
        integration_providers: providers,
        billing_provider: state.billing_provider.clone(),
        canonical_base_url: None,
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
        "authority_kind": "contract",
        "authority": authority,
        "calendar_role": "firm",
        "category": "internal",
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
    with_postgres(async {
        let fixture = fixture().await;
        let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
        let (status, created) = send(
            &fixture.app,
            "POST",
            &uri,
            &fixture.participant,
            body("Synthetic agreement § 1"),
        )
        .await;
        assert_eq!(status, 201, "{created}");
        assert_eq!(created["status"], "open");
        assert_eq!(created["calendar_role"], "firm");
        let id = created["id"].as_str().unwrap();

        let (status, listed) =
            send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
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
    })
    .await;
}

#[tokio::test]
async fn a_court_date_submitted_directly_requires_lawyer_review() {
    with_postgres(async {
        let fixture = fixture().await;
        let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
        let mut input: Value = serde_json::from_slice(&body("Synthetic order")).unwrap();
        input["authority_kind"] = json!("court_order");
        let (status, refused) = send(
            &fixture.app,
            "POST",
            &uri,
            &fixture.participant,
            serde_json::to_vec(&input).unwrap(),
        )
        .await;
        assert_eq!(status, 400, "{refused}");
        assert!(refused["message"]
            .as_str()
            .unwrap()
            .contains("review queue"));
        let (_, rows) = send(&fixture.app, "GET", &uri, &fixture.participant, Vec::new()).await;
        assert!(rows.as_array().unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    with_postgres(async {
        let fixture = fixture().await;
        let (status, body) = send(
            &fixture.app,
            "POST",
            &format!("/app/api/projects/{}/deadlines", fixture.project_id),
            &fixture.outsider,
            body("Synthetic agreement § 1"),
        )
        .await;
        assert_eq!(status, 404, "{body}");
    })
    .await;
}

#[tokio::test]
async fn civil_dates_survive_readback_and_cannot_be_combined_with_a_due_instant() {
    with_postgres(async {
        let fixture = fixture().await;
        let path = format!("/app/api/projects/{}/deadlines", fixture.project_id);
        let mut input: serde_json::Value = serde_json::from_slice(&body("Order")).unwrap();
        input["due_on"] = json!({"date":"2028-07-12","timezone":"America/Los_Angeles"});
        let (status, _) = send(
            &fixture.app,
            "POST",
            &path,
            &fixture.participant,
            serde_json::to_vec(&input).unwrap(),
        )
        .await;
        assert_eq!(status, 400);
        input.as_object_mut().unwrap().remove("due_at");
        let (status, row) = send(
            &fixture.app,
            "POST",
            &path,
            &fixture.participant,
            serde_json::to_vec(&input).unwrap(),
        )
        .await;
        assert_eq!(status, 201, "{row}");
        assert_eq!(row["due_on"], input["due_on"]);
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(row["due_at"].as_str().unwrap()).unwrap(),
            chrono::DateTime::parse_from_rfc3339("2028-07-13T00:00:00-07:00").unwrap()
        );
        let (status, inherited) = send(
            &fixture.app,
            "PATCH",
            &format!("{path}/{}", row["id"].as_str().unwrap()),
            &fixture.participant,
            serde_json::to_vec(&json!({"due_on":{"date":"2028-07-14"}})).unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{inherited}");
        assert_eq!(inherited["due_on"]["date"], "2028-07-14");
        assert_eq!(inherited["due_on"]["timezone"], "America/Los_Angeles");
        input["due_on"] = json!({"date":"2028-07-14"});
        let (status, new_row) = send(
            &fixture.app,
            "POST",
            &path,
            &fixture.participant,
            serde_json::to_vec(&input).unwrap(),
        )
        .await;
        assert_eq!(status, 201, "{new_row}");
        assert_eq!(new_row["due_on"]["timezone"], "America/New_York");
    })
    .await;
}

#[tokio::test]
async fn a_statutory_deadline_without_evidence_is_refused() {
    with_postgres(async {
        let fixture = fixture().await;
        let mut input: Value = serde_json::from_slice(&body("")).unwrap();
        input["authority_kind"] = json!("statute");
        let (status, response) = send(
            &fixture.app,
            "POST",
            &format!("/app/api/projects/{}/deadlines", fixture.project_id),
            &fixture.participant,
            serde_json::to_vec(&input).unwrap(),
        )
        .await;
        assert_eq!(status, 400, "{response}");
        assert_eq!(response["error"], "invalid_request");
    })
    .await;
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn cancelled_replay_requires_explicit_reopen_on_the_same_project() {
    with_postgres(async {
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
        assert_eq!(created["calendar_sequence"], 0);
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
        assert_eq!(cancelled["calendar_sequence"], 1);
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
        assert_eq!(reopened["calendar_sequence"], 2);
        let mut expected = created.clone();
        expected["calendar_sequence"] = json!(2);
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
    })
    .await;
}

#[tokio::test]
async fn a_title_is_stored_changed_and_cleared() {
    with_postgres(async {
        let fixture = fixture().await;
        let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
        let mut input: Value = serde_json::from_slice(&body("Synthetic agreement § 1")).unwrap();
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
    })
    .await;
}

#[tokio::test]
async fn a_proposal_patch_preserves_existing_deadline_metadata() {
    use base64::Engine as _;
    with_postgres(async {
        let fixture = fixture().await;
        let (status, document) = send(
        &fixture.app,
        "POST",
        &format!("/app/api/projects/{}/documents", fixture.project_id),
        &fixture.participant,
        serde_json::to_vec(&json!({
            "filename":"synthetic-notice.txt", "kind":"unclassified", "content_type":"text/plain",
            "content_base64":base64::engine::general_purpose::STANDARD.encode(b"Synthetic notice")
        }))
        .unwrap(),
    )
    .await;
        assert_eq!(status, 201, "{document}");
        let uri = format!("/app/api/projects/{}/deadlines", fixture.project_id);
        let mut input: Value = serde_json::from_slice(&body("Synthetic Rule 1")).unwrap();
        input["replay_key"] = json!("synthetic-rule-1");
        input["owner_person_id"] = json!(fixture.participant.person_id.unwrap());
        input["source_document_id"] = document["current_version"]["asset_id"].clone();
        input["source"] = json!("synthetic-workflow");
        input["calendar_role"] = json!("opponent");
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
        let (status, completed) = send(
            &fixture.app,
            "POST",
            &format!("{uri}/{id}/complete"),
            &fixture.participant,
            Vec::new(),
        )
        .await;
        assert_eq!(status, 200, "{completed}");
        let (status, updated) = send(
            &fixture.app,
            "PATCH",
            &format!("{uri}/{id}"),
            &fixture.participant,
            serde_json::to_vec(&json!({
                "title":"Synthetic response", "description":"Synthetic reviewed proposal audit",
                "due_at":"2028-03-01T17:00:00-08:00", "authority_kind":"contract",
                "authority":"Synthetic Rule 2"
            }))
            .unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{updated}");
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(updated["due_at"].as_str().unwrap()).unwrap(),
            chrono::DateTime::parse_from_rfc3339("2028-03-01T17:00:00-08:00").unwrap()
        );
        assert_eq!(updated["authority"], "Synthetic Rule 2");
        for field in [
            "id",
            "owner_person_id",
            "source_document_id",
            "internal_lead_days",
            "client_lead_days",
            "calendar_role",
            "source",
            "replay_key",
            "status",
        ] {
            assert_eq!(
                updated[field], completed[field],
                "proposal PATCH must retain {field}"
            );
        }
    })
    .await;
}

async fn file_synthetic_order(fixture: &Fixture, project_url: &str) -> Value {
    use base64::Engine as _;
    use image::{DynamicImage, ImageFormat};

    let mut original = std::io::Cursor::new(Vec::new());
    DynamicImage::new_rgb8(120, 40)
        .write_to(&mut original, ImageFormat::Png)
        .unwrap();
    let bytes = original.into_inner();
    let upload = json!({
        "filename": "synthetic-entered-order.png",
        "kind": "order",
        "content_type": "image/png",
        "content_base64": base64::engine::general_purpose::STANDARD.encode(&bytes)
    });
    let (status, document) = send(
        &fixture.app,
        "POST",
        &format!("{project_url}/documents"),
        &fixture.participant,
        serde_json::to_vec(&upload).unwrap(),
    )
    .await;
    assert_eq!(status, 201, "{document}");
    json!({
        "role": "signed_order",
        "id": document["current_version"]["asset_id"],
        "sha256_hex": store::assets::sha256_hex(&bytes),
        "page": 1,
        "rects": [{"x":0.1,"y":0.2,"width":0.7,"height":0.2}]
    })
}

async fn queue_order(fixture: &Fixture, queue: &str, evidence: &Value, date: &str) -> Value {
    let submission = json!({
        "request_id": Uuid::new_v4(),
        "draft": {"kind":"signed_order","title":"Synthetic response","due_on":date,"evidence":[evidence]}
    });
    let (status, row) = send(
        &fixture.app,
        "POST",
        queue,
        &fixture.participant,
        serde_json::to_vec(&submission).unwrap(),
    )
    .await;
    assert_eq!(status, 201, "{row}");
    row
}

async fn verify_cited_page(fixture: &Fixture, evidence_path: &str, revision: i64) {
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{evidence_path}?revision={revision}"))
                .extension(fixture.participant.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let png = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    let opener = format!("{evidence_path}/forgejo?revision={revision}");
    for session in [&fixture.participant, &fixture.outsider] {
        let (status, _) = send(&fixture.app, "GET", &opener, session, Vec::new()).await;
        assert_eq!(status, 404);
    }
}

/// Two staged dates stay out of the feed until a participating lawyer releases one revision.
#[tokio::test]
async fn a_filed_order_controls_which_queued_date_is_published() {
    with_postgres(review_filed_order()).await;
}

async fn review_filed_order() {
    use store::{deadline_proposals, postgres::PostgresDb};

    let fixture = fixture().await;
    let project_url = format!("/app/api/projects/{}", fixture.project_id);
    let queue = format!("{project_url}/court-date-proposals");
    let deadlines = format!("{project_url}/deadlines");
    let evidence = file_synthetic_order(&fixture, &project_url).await;
    let first = queue_order(&fixture, &queue, &evidence, "2030-07-16").await;
    let second = queue_order(&fixture, &queue, &evidence, "2030-08-01").await;
    assert_ne!(first["id"], second["id"]);
    let (status, empty) = send(
        &fixture.app,
        "GET",
        &deadlines,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!((status, empty), (200, json!([])));
    let (_, proposals) = send(
        &fixture.app,
        "GET",
        &queue,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    assert_eq!(
        proposals["proposals"][0]["evidence"][0]["label"],
        "synthetic-entered-order.png"
    );
    let id = first["id"].as_str().unwrap();
    let revision = first["revision"].as_i64().unwrap();
    let confirm = format!("{queue}/{id}/confirm");
    let decision = serde_json::to_vec(&json!({"revision":revision})).unwrap();
    let mut cli_session = fixture.participant.clone();
    cli_session.source = portal::session::SessionSource::Cli;
    for (session, expected) in [(&cli_session, 403), (&fixture.outsider, 404)] {
        let (status, _) = send(&fixture.app, "POST", &confirm, session, decision.clone()).await;
        assert_eq!(status, expected);
    }
    let stale = serde_json::to_vec(&json!({"revision":revision+1})).unwrap();
    let (status, _) = send(&fixture.app, "POST", &confirm, &fixture.participant, stale).await;
    assert_eq!(status, 409);
    verify_cited_page(&fixture, &format!("{queue}/{id}/evidence/0"), revision).await;
    let (status, accepted) = send(
        &fixture.app,
        "POST",
        &confirm,
        &fixture.participant,
        decision.clone(),
    )
    .await;
    assert_eq!(status, 200, "{accepted}");
    assert_eq!(accepted["status"], "approved");
    let pg = PostgresDb::application().await.unwrap();
    let job = deadline_proposals::Job {
        project_id: fixture.project_id,
        id: Uuid::parse_str(id).unwrap(),
        revision,
        attempt: 0,
    };
    deadline_proposals::publish(&pg, job).await.unwrap();
    deadline_proposals::publish(&pg, job).await.unwrap();
    let (status, replayed) = send(
        &fixture.app,
        "POST",
        &confirm,
        &fixture.participant,
        decision,
    )
    .await;
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["status"], "published");
    let (_, rows) = send(
        &fixture.app,
        "GET",
        &deadlines,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    let dates = rows.as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert_eq!(dates[0]["id"], replayed["deadline_id"]);
    assert_eq!(dates[0]["due_on"]["date"], "2030-07-16");
}
