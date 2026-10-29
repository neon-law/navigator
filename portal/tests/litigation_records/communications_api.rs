//! Communication doors (LAW-140): a participating Lawyer logs a call and
//! lists the firm view; a non-participant is refused with 404; the
//! client-visible projection excludes the call; an email logged with
//! `confirms` marks the call's agreements confirmed.

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
    client: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id =
        store::test_support::seed_project_surreal(&surreal, "Communication matter").await;
    let participant = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Lawyer", "call-lawyer@example.com", Role::Lawyer),
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
    let client = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Client", "call-client@example.com", Role::Client),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, client.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role("Other Lawyer", "call-outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal.clone()).await;
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
            ..portal::SessionData::fresh("call-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("call-outsider@example.com", Role::Lawyer)
        },
        client: portal::SessionData {
            person_id: Some(client.id),
            ..portal::SessionData::fresh("call-client@example.com", Role::Client)
        },
    }
}

fn a_call() -> Value {
    json!({
        "channel": "call",
        "direction": "outbound",
        "counterparty": "counsel@example.com",
        "role": "opposing_counsel",
        "at": "2026-10-09T15:00:00-07:00",
        "duration_minutes": 20,
        "body": "Discussed the discovery schedule.",
        "recorded": "none",
        "meet_and_confer": "Responses to Set One",
        "agreements": ["Responses extended to 2026-11-02"]
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
async fn a_participant_logs_a_call_lists_it_and_confirms_it_by_email() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/communications", f.project_id);
    let (status, created) = send(&f.app, "POST", &uri, &f.participant, a_call()).await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["channel"], "call");
    assert_eq!(created["direction"], "internal");
    assert_eq!(created["call"]["direction"], "outbound");
    assert_eq!(created["call"]["counterparty_role"], "opposing_counsel");
    assert_eq!(created["agreements"][0]["status"], "unconfirmed");
    let call_id = created["id"].as_str().unwrap().to_string();

    let (status, listed) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], call_id);

    // The client's projection of the conversation never carries the call.
    let (status, conversation) = send(
        &f.app,
        "GET",
        &format!("/app/api/projects/{}/conversation", f.project_id),
        &f.client,
        Value::Null,
    )
    .await;
    assert_eq!(status, 200, "{conversation}");
    assert!(
        conversation.as_array().unwrap().is_empty(),
        "{conversation}"
    );
    assert!(
        store::communications::for_project_client_visible(&f.surreal, f.project_id)
            .await
            .unwrap()
            .is_empty()
    );

    let (status, email) = send(
        &f.app,
        "POST",
        &uri,
        &f.participant,
        json!({
            "channel": "email_outbound",
            "direction": "outbound",
            "counterparty": "counsel@example.com",
            "at": "2026-10-09T16:00:00-07:00",
            "subject": "Confirming our call",
            "body": "As discussed, responses are extended to 2026-11-02.",
            "confirms": call_id
        }),
    )
    .await;
    assert_eq!(status, 201, "{email}");
    assert_eq!(email["channel"], "email_outbound");
    let (status, listed) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(status, 200);
    let call = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == call_id)
        .unwrap();
    assert_eq!(call["agreements"][0]["status"], "confirmed");
    assert_eq!(call["agreements"][0]["confirmed_by"], email["id"]);
}

#[tokio::test]
async fn a_non_participant_is_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/communications", f.project_id);
    let (status, body) = send(&f.app, "POST", &uri, &f.outsider, a_call()).await;
    assert_eq!(status, 404, "{body}");
    let (status, body) = send(&f.app, "GET", &uri, &f.outsider, Value::Null).await;
    assert_eq!(status, 404, "{body}");
}

#[tokio::test]
async fn an_opposing_counsel_call_cannot_be_client_visible_and_bad_values_are_refused() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/communications", f.project_id);
    let mut shared = a_call();
    shared["client_visible"] = json!(true);
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, shared).await;
    assert_eq!(status, 400, "{body}");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("always internal"));

    let mut role = a_call();
    role["role"] = json!("stranger");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, role).await;
    assert_eq!(status, 400, "{body}");

    let mut at = a_call();
    at["at"] = json!("yesterday");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, at).await;
    assert_eq!(status, 400, "{body}");

    let mut channel = a_call();
    channel["channel"] = json!("sms_inbound");
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, channel).await;
    assert_eq!(status, 400, "{body}");

    let mut confirms = a_call();
    confirms["channel"] = json!("email_outbound");
    confirms["confirms"] = json!(Uuid::now_v7());
    let (status, body) = send(&f.app, "POST", &uri, &f.participant, confirms).await;
    assert_eq!(
        status, 404,
        "a confirmation of no call is not found: {body}"
    );
}

#[tokio::test]
async fn email_receipt_links_preserve_roles_and_reject_invalid_targets_before_writing() {
    let f = fixture().await;
    let uri = format!("/app/api/projects/{}/communications", f.project_id);
    let sent = json!({"channel":"email_outbound", "direction":"outbound",
        "counterparty":"counsel@example.com", "role":"opposing_counsel",
        "at":"2026-10-09T15:00:00Z", "body":"Sent message", "source_ref":"gmail:synthetic/sent"});
    let (status, outbound) = send(&f.app, "POST", &uri, &f.participant, sent.clone()).await;
    assert_eq!(status, 201, "{outbound}");
    assert_eq!(outbound["counterparty_role"], "opposing_counsel");
    let mut receipt = sent.clone();
    receipt["channel"] = json!("email_inbound");
    receipt["direction"] = json!("inbound");
    receipt["at"] = json!("2026-10-09T16:00:00Z");
    receipt["source_ref"] = json!("gmail:synthetic/receipt");
    receipt["acknowledges"] = outbound["id"].clone();
    let (status, inbound) = send(&f.app, "POST", &uri, &f.participant, receipt.clone()).await;
    assert_eq!(status, 201, "{inbound}");
    assert_eq!(inbound["acknowledges"], outbound["id"]);
    assert_eq!(inbound["counterparty_role"], "opposing_counsel");
    let (status, replay) = send(&f.app, "POST", &uri, &f.participant, receipt.clone()).await;
    assert_eq!(status, 201, "{replay}");
    assert_eq!(replay["id"], inbound["id"]);
    let (_, rows) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(rows.as_array().unwrap().len(), 2);
    assert_eq!(
        rows[0]["receipt_acknowledged_at"],
        "2026-10-09T16:00:00+00:00"
    );

    let other_project =
        store::test_support::seed_project_surreal(&f.surreal, "Other synthetic project").await;
    let other = store::communications::ingest(
        &f.surreal,
        &store::communications::IngestArgs {
            project_id: other_project,
            channel: "email_outbound",
            direction: "outbound",
            author_person_id: None,
            counterparty: None,
            subject: None,
            body: "Other message",
            source_ref: None,
            asset_id: None,
            occurred_at: "2026-10-09T15:00:00Z",
        },
    )
    .await
    .unwrap();
    for target in [
        json!(Uuid::now_v7()),
        json!(other.communication_id),
        inbound["id"].clone(),
    ] {
        receipt["source_ref"] = Value::Null;
        receipt["acknowledges"] = target;
        let (status, error) = send(&f.app, "POST", &uri, &f.participant, receipt.clone()).await;
        assert!(status == 400 || status == 404, "{status}: {error}");
    }
    receipt["acknowledges"] = outbound["id"].clone();
    for (channel, direction, role) in [
        ("email_outbound", "outbound", "other"),
        ("email_inbound", "outbound", "other"),
        ("email_inbound", "inbound", "invalid"),
    ] {
        receipt["channel"] = json!(channel);
        receipt["direction"] = json!(direction);
        receipt["role"] = json!(role);
        let (status, error) = send(&f.app, "POST", &uri, &f.participant, receipt.clone()).await;
        assert_eq!(status, 400, "{error}");
    }
    let (_, rows) = send(&f.app, "GET", &uri, &f.participant, Value::Null).await;
    assert_eq!(
        rows.as_array().unwrap().len(),
        2,
        "invalid requests must not insert rows"
    );
}
