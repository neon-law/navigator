//! The integration provisioning doors under `/app/api/integrations/` —
//! Notion ensure/reconcile, Slack ensure/notify, and the calendar ensure —
//! behind an admin-tier login. Every scenario drives the full Axum router
//! against a `FakeIntegrations` provider set (or the unconfigured stand-in),
//! so the outcome slugs the `navigator integrations` surface renders are the
//! same strings the real provider adapters would report.

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
    surreal: SurrealDb,
    project_code: String,
    admin: portal::SessionData,
    lawyer: portal::SessionData,
}

async fn fixture() -> Fixture {
    fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::UnconfiguredIntegrations::default(),
    ))
    .await
}

async fn fixture_with_integrations(
    integrations: std::sync::Arc<dyn portal::integrations::IntegrationProviders>,
) -> Fixture {
    let db = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&db, "neon").await;
    let admin = ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Admin".into(),
            email: format!("integrations-admin-{}@example.com", Uuid::new_v4()),
            role: Role::Admin,
            ..Default::default()
        },
    )
    .await;
    store::firms::add_membership(
        &db,
        &NewPersonFirmRole {
            person_id: admin.id,
            firm_id,
            membership: FirmMembership::Admin,
            is_dri: false,
        },
    )
    .await
    .expect("enroll the synthetic admin in the seeded firm");
    let lawyer = ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: format!("integrations-lawyer-{}@example.com", Uuid::new_v4()),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    let (project_code, project_id) = seed_firm_project(&db, firm_id).await;
    // Project visibility is participation-scoped for every firm tier, so the
    // admin needs the same firm-side row a real operator would hold.
    store::participation::add_participant(
        &db,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: admin.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .expect("add the synthetic admin as a participant");
    let state = portal::test_support::app_state(db).await;
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
        integration_providers: integrations,
        billing_provider: state.billing_provider.clone(),
        canonical_base_url: None,
    });
    Fixture {
        app,
        surreal: state.surreal.clone(),
        project_code,
        admin: portal::SessionData {
            person_id: Some(admin.id),
            ..portal::SessionData::fresh(&admin.email, Role::Admin)
        },
        lawyer: portal::SessionData {
            person_id: Some(lawyer.id),
            ..portal::SessionData::fresh(&lawyer.email, Role::Lawyer)
        },
    }
}

/// A firm-owned, participating matter for the admin fixture: the shape
/// `authorize_project` demands (`firm_id` set) and `visible_for` admits.
async fn seed_firm_project(surreal: &SurrealDb, firm_id: uuid::Uuid) -> (String, uuid::Uuid) {
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic integrations jurisdiction {}", Uuid::new_v4()),
            format!("INT-{}", Uuid::new_v4()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(
        surreal,
        &format!("Synthetic integrations entity type {}", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: "Synthetic Integrations Matter Entity".into(),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: store::projects::code_from_name("Integrations matter", Uuid::new_v4()),
            name: "Integrations matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (project.code, project.id)
}

async fn post_json(
    app: &Router,
    uri: &str,
    body: Value,
    session: &portal::SessionData,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn first_result(body: &Value) -> &Value {
    body["results"]
        .as_array()
        .expect("an integrations report carries a results array")
        .first()
        .expect("the report carries exactly the selected projects")
}

#[tokio::test]
async fn a_lawyer_is_refused_at_the_admin_extractor() {
    let f = fixture().await;
    let response = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        json!({ "all": true }),
        &f.lawyer,
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn an_ambiguous_or_empty_selector_is_a_malformed_request() {
    let f = fixture().await;
    for selector in [
        json!({ "project_code": f.project_code, "all": true }),
        json!({}),
    ] {
        let response = post_json(
            &f.app,
            "/app/api/integrations/notion/ensure",
            selector,
            &f.admin,
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert_eq!(body["error"], "invalid_request");
    }
}

#[tokio::test]
async fn an_unconfigured_runtime_reports_the_slug_instead_of_provisioning() {
    let f = fixture().await;
    let response = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        json!({ "project_code": f.project_code }),
        &f.admin,
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = body_json(response).await;
    let result = first_result(&body);
    assert_eq!(result["project_code"], f.project_code.as_str());
    assert_eq!(result["outcome"], "runtime_not_configured");
    let detail = result["detail"]
        .as_str()
        .expect("the outcome names its gap");
    assert!(detail.contains("NAVIGATOR_RUNTIME_KMS_KEY"), "{detail}");
}

#[tokio::test]
async fn the_first_ensure_creates_the_page_and_records_its_address() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    let response = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        json!({ "project_code": f.project_code }),
        &f.admin,
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(first_result(&body)["outcome"], "created");
    // The row is what every later surface reads: the address must have
    // landed on the Project itself, not only in the door's report.
    let project = store::projects::find_by_code(&f.surreal, &f.project_code)
        .await
        .unwrap()
        .expect("the fixture's project exists");
    assert!(project.private_notion_page_url.is_some());
}

#[tokio::test]
async fn a_second_ensure_adopts_the_recorded_page_instead_of_duplicating_it() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    let selector = json!({ "project_code": f.project_code });
    let first = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        selector.clone(),
        &f.admin,
    )
    .await;
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    assert_eq!(first_result(&body_json(first).await)["outcome"], "created");
    let second = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        selector,
        &f.admin,
    )
    .await;
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    assert_eq!(
        first_result(&body_json(second).await)["outcome"],
        "adopted",
        "the recorded page is validated by id and adopted, never recreated"
    );
}

#[tokio::test]
async fn reconcile_reports_unchanged_for_the_recorded_page() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    // Provision through ensure first: reconcile never provisions, so a
    // matter with no recorded page can only be reported, not repaired.
    let ensured = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        json!({ "project_code": f.project_code }),
        &f.admin,
    )
    .await;
    assert_eq!(ensured.status(), axum::http::StatusCode::OK);
    let reconciled = post_json(
        &f.app,
        "/app/api/integrations/notion/reconcile",
        json!({ "project_code": f.project_code }),
        &f.admin,
    )
    .await;
    assert_eq!(reconciled.status(), axum::http::StatusCode::OK);
    assert_eq!(
        first_result(&body_json(reconciled).await)["outcome"],
        "unchanged"
    );
}

#[tokio::test]
async fn the_calendar_ensure_creates_then_reconciles_the_shared_calendar() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    let selector = json!({ "project_code": f.project_code });
    let first = post_json(
        &f.app,
        "/app/api/integrations/calendar/ensure",
        selector.clone(),
        &f.admin,
    )
    .await;
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    assert_eq!(first_result(&body_json(first).await)["outcome"], "created");
    let second = post_json(
        &f.app,
        "/app/api/integrations/calendar/ensure",
        selector,
        &f.admin,
    )
    .await;
    assert_eq!(second.status(), axum::http::StatusCode::OK);
    assert_eq!(first_result(&body_json(second).await)["outcome"], "present");
}

#[tokio::test]
async fn the_contacts_sync_reports_counts_and_converges() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    let selector = json!({ "project_code": f.project_code });
    let first = post_json(
        &f.app,
        "/app/api/integrations/contacts/sync",
        selector.clone(),
        &f.admin,
    )
    .await;
    assert_eq!(first.status(), axum::http::StatusCode::OK);
    let first = first_result(&body_json(first).await).clone();
    assert_eq!(first["outcome"], "synced");
    let second = post_json(
        &f.app,
        "/app/api/integrations/contacts/sync",
        selector,
        &f.admin,
    )
    .await;
    let second = first_result(&body_json(second).await).clone();
    assert!(
        second["detail"]
            .as_str()
            .unwrap()
            .ends_with("0 created, 0 updated, 0 removed"),
        "a re-run changes nothing: {second}"
    );
}

#[tokio::test]
async fn slack_notify_rejects_an_event_outside_the_closed_vocabulary() {
    let f = fixture().await;
    let response = post_json(
        &f.app,
        "/app/api/integrations/slack/notify",
        json!({ "project_code": f.project_code, "event": "client says hi" }),
        &f.admin,
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["error"], "invalid_request");
    // The refusal happens before any provider resolution: the message names
    // the closed vocabulary rather than echoing the rejected value.
    assert!(body["message"].as_str().unwrap().contains("project_opened"));
}

#[tokio::test]
async fn slack_notify_posts_to_the_ensured_channel() {
    let f = fixture_with_integrations(std::sync::Arc::new(
        portal::integrations::FakeIntegrations::new(),
    ))
    .await;
    // Notify never provisions: ensure the Firm-private channel first, then
    // deliver the notice into the channel that pass created.
    let ensured = post_json(
        &f.app,
        "/app/api/integrations/slack/ensure",
        json!({ "project_code": f.project_code }),
        &f.admin,
    )
    .await;
    assert_eq!(ensured.status(), axum::http::StatusCode::OK);
    assert_eq!(
        first_result(&body_json(ensured).await)["outcome"],
        "created"
    );
    let notified = post_json(
        &f.app,
        "/app/api/integrations/slack/notify",
        json!({ "project_code": f.project_code, "event": "project_opened" }),
        &f.admin,
    )
    .await;
    assert_eq!(notified.status(), axum::http::StatusCode::OK);
    assert_eq!(
        first_result(&body_json(notified).await)["outcome"],
        "notified"
    );
}

#[tokio::test]
async fn a_project_outside_the_lens_is_not_yours_to_provision() {
    let f = fixture().await;
    // A code nobody can see gets the same answer an unknown code gets: the
    // 404 carries no hint whether the matter exists.
    let response = post_json(
        &f.app,
        "/app/api/integrations/notion/ensure",
        json!({ "project_code": "not-a-matter-you-can-see" }),
        &f.admin,
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::NOT_FOUND);
}
