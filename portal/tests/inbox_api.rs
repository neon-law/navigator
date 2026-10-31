//! `GET /app/api/inbox/unprocessed` and `POST /app/api/inbox/process` — the
//! backlog, plan, and record doors behind `inbox process` (LAW-166). Messages
//! are seeded through `store::inbox::ingest_inbound`, the same normalized
//! store the `SendGrid` webhook feeds, so the doors are exercised against the
//! transport facts the classifier actually reads. The Slack notice is
//! never expected to land here: every fixture message belongs to no matter.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::firms::{FirmMembership, NewPersonFirmRole};
use store::persons::{NewPerson, Role};
use store::surreal::SurrealDb;
use store::test_support::{ensure_person, mem_surreal, seed_firm_with_brand};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    lawyer_session: portal::SessionData,
    surreal: SurrealDb,
    firm_id: Uuid,
}

async fn fixture() -> Fixture {
    let db = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&db, "neon").await;
    let lawyer = ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: "synthetic-lawyer@example.com".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    store::firms::add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .expect("enroll the synthetic lawyer in the seeded firm");
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
        canonical_base_url: None,
    };
    let session = portal::SessionData {
        person_id: Some(lawyer.id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };
    Fixture {
        app: portal::api::routes().with_state(api_state),
        lawyer_session: session,
        surreal: state.surreal,
        firm_id,
    }
}

/// Seed one inbound message through the same normalized ingest the `SendGrid`
/// webhook feeds, returning the message id the doors address.
async fn seed_message(
    db: &SurrealDb,
    firm_id: Uuid,
    provider_message_id: &str,
    external_address: &str,
    subject: &str,
    body: &str,
) -> Uuid {
    let ingested = store::inbox::ingest_inbound(
        db,
        &store::inbox::IngestInbound {
            project_id: None,
            firm_id,
            brand_key: None,
            interaction_type: Some("message"),
            provider_post_id: None,
            parent_provider_message_id: None,
            visibility: None,
            channel: store::inbox::channel::EMAIL,
            provider_account: "support",
            provider_message_id,
            provider_thread_ref: None,
            external_address,
            external_name: None,
            subject: Some(subject),
            body,
            source_page: Some("support"),
            source_channel: Some(store::inbox::channel::EMAIL),
            matter_consent: false,
            promotional_opt_in: false,
            email_conversation_id: None,
            attachments: Vec::new(),
            occurred_at: &chrono::Utc::now().to_rfc3339(),
        },
    )
    .await
    .expect("ingest the synthetic inbound message");
    ingested.message_id
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    session: Option<&portal::SessionData>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(session) = session {
        builder = builder.extension(session.clone());
    }
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize synthetic request"))
        .unwrap_or_default();
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).expect("build request"))
        .await
        .expect("run request");
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read response body");
    let json: Value = serde_json::from_slice(&bytes).expect("response is JSON");
    (status, json)
}

const NEF_ADDRESS: &str = "United States Bankruptcy Court, N.D. Cal. <ECF_Reply@uscourts.gov>";
const NEF_SUBJECT: &str = "Notice of Electronic Filing";
const NEF_BODY: &str = "This is an automatic email message from the CM/ECF system.";
const CLAIMS_AGENT_ADDRESS: &str = "Dawn Branch <notices@kroll.com>";
const OTHER_ADDRESS: &str = "Jordan Field <jordan@example.com>";

#[tokio::test]
async fn the_backlog_classifies_fresh_and_serves_the_plan_shape() {
    let fixture = fixture().await;
    seed_message(
        &fixture.surreal,
        fixture.firm_id,
        "nef-1",
        NEF_ADDRESS,
        NEF_SUBJECT,
        NEF_BODY,
    )
    .await;
    seed_message(
        &fixture.surreal,
        fixture.firm_id,
        "claims-1",
        CLAIMS_AGENT_ADDRESS,
        "Notice of claims deadline",
        "A proof of claim deadline is approaching.",
    )
    .await;
    seed_message(
        &fixture.surreal,
        fixture.firm_id,
        "other-1",
        OTHER_ADDRESS,
        "Question about engagement",
        "Hello, I have a question.",
    )
    .await;

    let (status, body) = request(
        &fixture.app,
        "GET",
        "/app/api/inbox/unprocessed",
        Some(&fixture.lawyer_session),
        None,
    )
    .await;

    assert_eq!(status, 200, "the backlog answers 200: {body}");
    let rows = body.as_array().expect("the backlog is a JSON array");
    assert_eq!(rows.len(), 3, "every unprocessed message is listed");
    let routes = rows
        .iter()
        .map(|row| row["computed_route"].as_str().expect("route is a string"))
        .collect::<Vec<_>>();
    assert!(routes.contains(&"nef"), "the NEF message routes to nef");
    assert!(
        routes.contains(&"claims_agent"),
        "the claims-agent message routes to claims_agent"
    );
    assert!(
        routes.contains(&"other"),
        "the unmatched message routes to other"
    );
    for row in rows {
        assert_eq!(
            row["recorded_route"],
            Value::Null,
            "nothing is recorded yet"
        );
        assert_eq!(row["channel"], "email", "the message arrived over email");
        assert!(
            row["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "every plan carries the marker the classifier matched"
        );
    }
}

#[tokio::test]
async fn the_plan_door_reads_without_recording() {
    let fixture = fixture().await;
    let message = seed_message(
        &fixture.surreal,
        fixture.firm_id,
        "nef-plan-1",
        NEF_ADDRESS,
        NEF_SUBJECT,
        NEF_BODY,
    )
    .await;

    let (status, body) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": false })),
    )
    .await;

    assert_eq!(status, 200, "the plan answers 200: {body}");
    assert_eq!(body["computed_route"], "nef");
    assert_eq!(
        body["recorded_route"],
        Value::Null,
        "a plan records nothing"
    );
    assert_eq!(body["recorded_at"], Value::Null);
    assert_eq!(body["external_address"], NEF_ADDRESS.to_string());

    let (status, backlog) = request(
        &fixture.app,
        "GET",
        "/app/api/inbox/unprocessed",
        Some(&fixture.lawyer_session),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        backlog.as_array().map(Vec::len),
        Some(1),
        "the message stays on the backlog"
    );
}

#[tokio::test]
async fn apply_records_the_route_and_a_rerun_reports_already_recorded() {
    let fixture = fixture().await;
    let message = seed_message(
        &fixture.surreal,
        fixture.firm_id,
        "nef-apply-1",
        NEF_ADDRESS,
        NEF_SUBJECT,
        NEF_BODY,
    )
    .await;

    let (status, first) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the apply answers 200: {first}");
    assert_eq!(first["route"], "nef", "the storage spelling is served");
    assert_eq!(
        first["route_label"], "NEF",
        "the label is served beside the route"
    );
    assert_eq!(
        first["already_recorded"], false,
        "the first run records fresh"
    );
    assert_eq!(
        first["slack_notice"], "no_project",
        "a matterless message tells no channel"
    );
    assert_eq!(
        first["result"]["recorded_route"], "nef",
        "the result view carries the recording"
    );

    let (status, again) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": message, "apply": true })),
    )
    .await;

    assert_eq!(status, 200, "the rerun answers 200: {again}");
    assert_eq!(
        again["already_recorded"], true,
        "the rerun reports the earlier recording"
    );
    assert_eq!(again["route"], "nef", "the rerun records the same truth");
}

#[tokio::test]
async fn an_unknown_message_is_not_found() {
    let fixture = fixture().await;

    let (status, body) = request(
        &fixture.app,
        "POST",
        "/app/api/inbox/process",
        Some(&fixture.lawyer_session),
        Some(json!({ "message": Uuid::new_v4(), "apply": false })),
    )
    .await;

    assert_eq!(status, 404, "an unknown message answers 404: {body}");
}
