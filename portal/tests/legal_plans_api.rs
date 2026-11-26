//! A Firm's Legal Plans and a Project's plan link: Firm members read, only the
//! Firm's administrators add plans, and a matter links only to a plan of its
//! own Firm by name, with its Plan Case Number.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
async fn plans_are_firm_data_and_a_matter_links_to_its_own_firms_plan() {
    store::test_support::with_postgres(run()).await;
}

async fn send(
    app: &Router,
    session: &portal::SessionData,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .extension(session.clone());
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/json");
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn api(surreal: store::surreal::SurrealDb) -> Router {
    let state = portal::test_support::app_state(surreal).await;
    portal::api::routes().with_state(portal::api::ApiState {
        jev: None,
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
    })
}

fn session(person: &store::persons::Person, role: Role) -> portal::SessionData {
    portal::SessionData {
        person_id: Some(person.id),
        ..portal::SessionData::fresh(person.email.clone(), role)
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one scenario walks every door of the plan vocabulary and the matter link"
)]
async fn run() {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    let surreal = store::surreal::SurrealDb::uninitialized();
    sqlx::query("INSERT INTO brand(name, brand_key) VALUES ('Synthetic Brand', 'neon')")
        .execute(pg.pool())
        .await
        .unwrap();
    let entity_id: Uuid = sqlx::query_scalar(
        "WITH kind AS (INSERT INTO entity_type(name) VALUES ('Plan Entity Type') RETURNING id),
          place AS (INSERT INTO jurisdiction(name) VALUES ('Plan Jurisdiction') RETURNING id)
          INSERT INTO entity(name,entity_type_id,jurisdiction_id)
          SELECT 'Synthetic Plan Entity',kind.id,place.id FROM kind, place RETURNING id",
    )
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let owner = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Owner", "plans-owner@example.com", Role::Owner),
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Outsider",
            "plans-outsider@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: format!("Synthetic Plan Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id,
            admin_dri_person_id: owner.id,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: store::projects::code_from_name("Synthetic Plan Matter", Uuid::new_v4()),
            name: "Synthetic Plan Matter".into(),
            status: "open".into(),
            entity_id,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let firmless = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: store::projects::code_from_name("Synthetic Firmless Matter", Uuid::new_v4()),
            name: "Synthetic Firmless Matter".into(),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let app = api(surreal).await;
    let owner = session(&owner, Role::Owner);
    let outsider = session(&outsider, Role::Lawyer);
    let plans = format!("/app/api/firms/{}/legal-plans", firm.id);
    let link = format!("/app/api/projects/{}/legal-plan", project.id);

    let arag = json!({
        "name": "ARAG",
        "portal": "https://plan.example.com/account",
        "contact": "provider-support@plan.example",
        "acceptance_sla_hours": 24,
        "turnaround_sla_days": 90
    });
    let (status, created) = send(&app, &owner, "POST", &plans, Some(arag.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["portal"], "https://plan.example.com/account");
    assert_eq!(
        send(&app, &owner, "POST", &plans, Some(arag)).await.0,
        StatusCode::CONFLICT
    );
    let (status, refused) = send(
        &app,
        &owner,
        "POST",
        &plans,
        Some(json!({"name": "Bad", "acceptance_sla_hours": 0, "turnaround_sla_days": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused["field"], "acceptance_sla_hours");
    let (status, listed) = send(&app, &owner, "GET", &plans, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed.as_array().map(Vec::len), Some(1));

    // Someone outside the Firm learns nothing: not its plans, not its matter.
    assert_eq!(
        send(&app, &outsider, "GET", &plans, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            &outsider,
            "POST",
            &plans,
            Some(json!({"name": "X", "acceptance_sla_hours": 1, "turnaround_sla_days": 1}))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&app, &outsider, "GET", &link, None).await.0,
        StatusCode::NOT_FOUND
    );

    // An unlinked matter reads with no plan; linking names the plan in any case.
    let (status, unlinked) = send(&app, &owner, "GET", &link, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unlinked["legal_plan"], Value::Null);
    let (status, case_recorded) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(json!({"plan_case_number": "4900000-001"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{case_recorded}");
    assert_eq!(case_recorded["legal_plan"], Value::Null);
    let manifest_request = |name, dry_run| {
        json!({
            "manifest_legal_plan": {
                "name": name,
                "firm_id": firm.id,
                "brand": project.brand,
                "dry_run": dry_run,
            }
        })
    };
    assert_eq!(
        send(
            &app,
            &outsider,
            "PATCH",
            &link,
            Some(manifest_request("arag", false))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(manifest_request("arag", true))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, &owner, "GET", &link, None).await.1["legal_plan"],
        Value::Null
    );
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(manifest_request("arag", false))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (status, linked_from_manifest) = send(&app, &owner, "GET", &link, None).await;
    assert_eq!(status, StatusCode::OK, "{linked_from_manifest}");
    assert_eq!(linked_from_manifest["legal_plan"]["name"], "ARAG");
    assert_eq!(linked_from_manifest["plan_case_number"], "4900000-001");
    let (status, linked) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(
            json!({"legal_plan": "arag", "plan_case_number": " 4900000-001 ", "plan_state": "wa"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{linked}");
    assert_eq!(linked["legal_plan"]["name"], "ARAG");
    assert_eq!(linked["plan_case_number"], "4900000-001");
    // A later patch changes only what it names.
    let (status, accepted) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(json!({"plan_accepted_on": "2026-11-04", "plan_matter_type": "contested_divorce"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{accepted}");
    assert_eq!(accepted["plan_matter_type"], "contested_divorce");
    assert_eq!(accepted["plan_case_number"], "4900000-001");
    assert_eq!(accepted["plan_state"], "wa");
    assert_eq!(accepted["plan_accepted_on"], "2026-11-04");
    let (status, read) = send(&app, &owner, "GET", &link, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, accepted);

    assert_eq!(
        send(
            &app,
            &owner,
            "POST",
            &plans,
            Some(json!({
                "name": "Other Panel", "acceptance_sla_hours": 24, "turnaround_sla_days": 90
            }))
        )
        .await
        .0,
        StatusCode::CREATED
    );
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(manifest_request("Other Panel", false))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(manifest_request("arag", false))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(send(&app, &owner, "GET", &link, None).await.1, accepted);
    // A plan the Firm does not hold is refused whole, naming the verbs that
    // list and add the Firm's plans; a malformed number is refused too.
    let (status, refused) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(json!({
            "legal_plan": "Elsewhere", "plan_case_number": "4900000-002", "plan_member_id": "M-0002"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = refused["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("navigator legal-plans --firm <firm> list"),
        "{refused}"
    );
    assert!(message.contains("create --name <plan>"), "{refused}");
    assert_eq!(send(&app, &owner, "GET", &link, None).await.1, accepted);
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(json!({"plan_case_number": "4900000 001"}))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    // An empty value clears; a matter with no Firm has no plan vocabulary.
    let (status, cleared) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(json!({"legal_plan": "", "plan_case_number": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["legal_plan"], Value::Null);
    assert_eq!(cleared["plan_case_number"], Value::Null);
    assert_eq!(cleared["plan_state"], "wa");
    assert_eq!(
        send(
            &app,
            &owner,
            "GET",
            &format!("/app/api/projects/{}/legal-plan", firmless.id),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    verify_legal_ease_benefit(&app, &owner, &plans, &link).await;
}

async fn verify_legal_ease_benefit(
    app: &Router,
    owner: &portal::SessionData,
    plans: &str,
    link: &str,
) {
    let (status, created) = send(
        app,
        owner,
        "POST",
        plans,
        Some(json!({"name": "Legal Ease"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let terms = json!({
        "cap_shape": "hourly_cap",
        "cap_minutes": 240,
        "cap_cents": null,
        "contested_cap_cents": null,
        "plan_rate_cents_per_hour": 9000,
        "paralegal_rate_cents_per_hour": 4500
    });
    let (status, updated) = send(
        app,
        owner,
        "PATCH",
        link,
        Some(json!({
            "legal_plan": "LegalEASE",
            "benefit_code": "SAMPLE-BENEFIT",
            "benefit_terms": terms
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["legal_plan"]["name"], "Legal Ease");
    assert_eq!(updated["benefit_code"], "SAMPLE-BENEFIT");
    assert_eq!(updated["benefit_terms"], terms);
    assert_eq!(send(app, owner, "GET", link, None).await, (status, updated));
}
