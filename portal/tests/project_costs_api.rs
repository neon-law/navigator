//! Project cost routes enforce role and matter participation before opening the cost store.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgSslMode},
    PgPool,
};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    project_id: Uuid,
    surreal: store::surreal::SurrealDb,
    assets_storage: std::sync::Arc<dyn cloud::StorageService>,
    client: portal::SessionData,
    lawyer: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = source_project(&surreal).await;
    let client = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Client", "cost-client@example.com", Role::Client),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: client.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role("Other Lawyer", "cost-outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Project Lawyer", "cost-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
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
        canonical_base_url: None,
    });
    Fixture {
        app,
        project_id,
        surreal: state.surreal.clone(),
        assets_storage: state.assets_storage.clone(),
        client: portal::SessionData {
            person_id: Some(client.id),
            ..portal::SessionData::fresh("cost-client@example.com", Role::Client)
        },
        lawyer: portal::SessionData {
            person_id: Some(lawyer.id),
            ..portal::SessionData::fresh("cost-lawyer@example.com", Role::Lawyer)
        },
        outsider: portal::SessionData {
            person_id: Some(outsider.id),
            ..portal::SessionData::fresh("cost-outsider@example.com", Role::Lawyer)
        },
    }
}

async fn source_project(surreal: &store::surreal::SurrealDb) -> Uuid {
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            "Synthetic cost jurisdiction",
            format!("COST-{}", Uuid::new_v4()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(
        surreal,
        &format!("Synthetic cost entity type {}", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: "Synthetic Cost Matter Entity".into(),
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
            code: store::projects::code_from_name("Cost API matter", Uuid::new_v4()),
            name: "Cost API matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

#[tokio::test]
async fn lawyer_cost_lifecycle_and_client_receipt_visibility() {
    if std::env::var("NAV_COST_API_CHILD").as_deref() == Ok("1") {
        run_lifecycle().await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lawyer_cost_lifecycle_and_client_receipt_visibility",
            "--nocapture",
        ])
        .env("NAV_COST_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_lifecycle() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let receipt = store::documents::ingest_bytes_as(
        &fixture.surreal,
        &fixture.assets_storage,
        &store::documents::IngestArgs {
            project_id: fixture.project_id,
            source: store::documents::source::UPLOAD,
            filename: "receipt.pdf",
            kind: "exhibit",
            content_type: "application/pdf",
            description: Some("Synthetic filing receipt"),
            visibility: store::documents::visibility::CLIENT,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity::default(),
        b"synthetic receipt bytes",
    )
    .await
    .unwrap();
    let mut input = payload();
    input["receipt_document_id"] = json!(receipt.asset_id);
    let base = format!("/app/api/projects/{}/costs", fixture.project_id);
    let id = create_and_check_idempotency(&fixture, &base, input).await;
    assert_client_can_see_cost(&fixture, &base).await;
    confirm_audit_and_resolve(&fixture, &base, id).await;
    let invoice_cost = create_and_link_invoice_cost(&fixture, &base).await;
    assert_ne!(invoice_cost, id);
    store::assets::set_visibility(
        &fixture.surreal,
        fixture.project_id,
        receipt.asset_id,
        store::documents::visibility::INTERNAL,
    )
    .await
    .unwrap();
    let hidden = json_body(request(&fixture.app, "GET", base, &fixture.client, None).await).await;
    assert_eq!(hidden["costs"].as_array().unwrap().len(), 1);
    assert_eq!(hidden["totals"][0]["amount_cents"], 1250);
}

async fn create_and_check_idempotency(fixture: &Fixture, base: &str, mut input: Value) -> Uuid {
    let created = request(
        &fixture.app,
        "POST",
        base.to_owned(),
        &fixture.lawyer,
        Some(input.clone()),
    )
    .await;
    let created_status = created.status();
    let row = json_body(created).await;
    assert_eq!(created_status, StatusCode::CREATED, "{row}");
    let id = Uuid::parse_str(row["id"].as_str().unwrap()).unwrap();
    let replay = request(
        &fixture.app,
        "POST",
        base.to_owned(),
        &fixture.lawyer,
        Some(input.clone()),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(json_body(replay).await["id"], id.to_string());
    input["amount_cents"] = json!(1300);
    let conflict = request(
        &fixture.app,
        "POST",
        base.to_owned(),
        &fixture.lawyer,
        Some(input),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    id
}

async fn assert_client_can_see_cost(fixture: &Fixture, base: &str) {
    let response = request(&fixture.app, "GET", base.to_owned(), &fixture.client, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let rows = json_body(response).await;
    assert_eq!(rows["costs"].as_array().unwrap().len(), 1);
    assert_eq!(rows["totals"][0]["amount_status"], "estimated");
}

async fn confirm_audit_and_resolve(fixture: &Fixture, base: &str, id: Uuid) {
    let events = request(
        &fixture.app,
        "GET",
        format!("{base}/{id}/events"),
        &fixture.lawyer,
        None,
    )
    .await;
    assert_eq!(events.status(), StatusCode::OK);
    assert_eq!(
        json_body(events).await["events"].as_array().unwrap().len(),
        1
    );
    let confirmed = request(
        &fixture.app,
        "POST",
        format!("{base}/{id}/confirm"),
        &fixture.lawyer,
        Some(json!({"amount_cents": 1250})),
    )
    .await;
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(json_body(confirmed).await["amount_status"], "confirmed");
    let resolved = request(
        &fixture.app,
        "POST",
        format!("{base}/{id}/resolve"),
        &fixture.lawyer,
        Some(json!({"reason": "Synthetic cost settled outside the invoice"})),
    )
    .await;
    assert_eq!(resolved.status(), StatusCode::OK);
    assert_eq!(json_body(resolved).await["billing_status"], "resolved");
}

async fn create_and_link_invoice_cost(fixture: &Fixture, base: &str) -> Uuid {
    let invoice_id = format!("synthetic-invoice-{}", Uuid::new_v4());
    store::xero_invoices::upsert(
        &fixture.surreal,
        &store::xero_invoices::UpsertXeroInvoice {
            project_id: fixture.project_id,
            xero_invoice_id: invoice_id.clone(),
            reference: format!("Matter {}", fixture.project_id),
            status: "AUTHORISED".into(),
            amount_cents: 10_000,
            currency: "USD".into(),
            issued_at: chrono::Utc::now(),
            due_at: None,
        },
    )
    .await
    .unwrap();
    let receipt = store::documents::ingest_bytes_as(
        &fixture.surreal,
        &fixture.assets_storage,
        &store::documents::IngestArgs {
            project_id: fixture.project_id,
            source: store::documents::source::UPLOAD,
            filename: "second-receipt.pdf",
            kind: "exhibit",
            content_type: "application/pdf",
            description: Some("Second synthetic receipt"),
            visibility: store::documents::visibility::CLIENT,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity::default(),
        b"second synthetic receipt bytes",
    )
    .await
    .unwrap();
    let mut input = payload();
    input["receipt_document_id"] = json!(receipt.asset_id);
    input["idempotency_key"] = json!(format!("invoice-cost-{}", Uuid::new_v4()));
    let created = request(
        &fixture.app,
        "POST",
        base.to_owned(),
        &fixture.lawyer,
        Some(input),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let id = Uuid::parse_str(json_body(created).await["id"].as_str().unwrap()).unwrap();
    let linked = request(
        &fixture.app,
        "POST",
        format!("{base}/{id}/invoice"),
        &fixture.lawyer,
        Some(json!({"invoice_id": invoice_id})),
    )
    .await;
    assert_eq!(linked.status(), StatusCode::OK);
    assert_eq!(json_body(linked).await["billing_status"], "invoiced");
    id
}

async fn json_body(response: axum::response::Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()
}

async fn request(
    app: &Router,
    method: &'static str,
    uri: String,
    session: &portal::SessionData,
    body: Option<Value>,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn isolated_database() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options).await.unwrap();
    let database = format!("cost_api_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    Some((port, database))
}

async fn remove_isolated_database(port: &str, database: &str) {
    let admin = PgPool::connect_with(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database("smoke")
            .ssl_mode(PgSslMode::Disable),
    )
    .await
    .unwrap();
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(database)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}

fn payload() -> Value {
    json!({
        "occurred_on": "2026-10-16",
        "payee": "Synthetic Vendor",
        "description": "Synthetic filing copy",
        "amount_cents": 1250,
        "currency": "USD",
        "amount_status": "estimated",
        "receipt_document_id": Uuid::new_v4(),
        "idempotency_key": "synthetic-api-cost"
    })
}

#[tokio::test]
async fn client_cannot_create_a_cost_and_outsider_lawyer_cannot_cross_matter_scope() {
    let fixture = fixture().await;
    let uri = format!("/app/api/projects/{}/costs", fixture.project_id);
    let client_response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.client.clone())
                .body(Body::from(serde_json::to_vec(&payload()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(client_response.status().as_u16(), 403);

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.outsider.clone())
                .body(Body::from(serde_json::to_vec(&payload()).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 404);
}
