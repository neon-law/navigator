//! A Firm's Legal Plans and a Project's plan link: Firm members read, only the
//! Firm's administrators add plans, and a matter links only to a plan of its
//! own Firm by name, with its Plan Case Number.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::{
    postgres::{PgConnectOptions, PgSslMode},
    PgPool,
};
use store::persons::{NewPerson, Role};
use tower::ServiceExt;
use uuid::Uuid;

const CHILD: &str = "NAV_LEGAL_PLANS_API_CHILD";

fn admin_options(port: &str, database: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(database)
        .ssl_mode(PgSslMode::Disable)
}

#[tokio::test]
async fn plans_are_firm_data_and_a_matter_links_to_its_own_firms_plan() {
    if std::env::var(CHILD).as_deref() == Ok("1") {
        run().await;
        return;
    }
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL fixture required"
        );
        return;
    };
    let smoke =
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into());
    let database = format!("legal_plans_api_{}", Uuid::new_v4().simple());
    let admin = PgPool::connect_with(admin_options(&port, &smoke))
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    // The PostgreSQL store is read from the environment, so the scenario runs
    // in a child process that owns it rather than racing other tests' env.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "plans_are_firm_data_and_a_matter_links_to_its_own_firms_plan",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(&database)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    assert!(status.success());
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

/// A deployment whose `web` has no PostgreSQL connection still reads a
/// matter no plan referred, and names the missing plan store instead of
/// answering `500 internal` where it needs one.
#[tokio::test]
async fn without_postgresql_an_unlinked_matter_reads_and_a_write_names_the_store() {
    if std::env::var("NAVIGATOR_POSTGRES_HOST").is_ok() {
        return;
    }
    let surreal = store::test_support::mem_surreal().await;
    let owner = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Owner", "unlinked-owner@example.com", Role::Owner),
    )
    .await
    .unwrap();
    let entity_id = store::test_support::seed_entity(&surreal).await;
    let firm = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: format!("Synthetic Unlinked Firm {}", Uuid::now_v7()),
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
            code: store::projects::code_from_name("Synthetic Unlinked Matter", Uuid::new_v4()),
            name: "Synthetic Unlinked Matter".into(),
            status: "open".into(),
            entity_id,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let app = api(surreal).await;
    let owner = session(&owner, Role::Owner);
    let link = format!("/app/api/projects/{}/legal-plan", project.id);

    let (status, unlinked) = send(&app, &owner, "GET", &link, None).await;
    assert_eq!(status, StatusCode::OK, "{unlinked}");
    assert_eq!(unlinked["legal_plan"], Value::Null);
    assert_eq!(unlinked["code"], project.code);

    let (status, refused) = send(
        &app,
        &owner,
        "PATCH",
        &link,
        Some(json!({"plan_case_number": "4900000-001"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused["error"], "legal_plans_unavailable");
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
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let surreal = store::test_support::mem_surreal().await;
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
    let entity_id = store::test_support::seed_entity(&surreal).await;
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
    // The plan rows key the PostgreSQL firm row; the Firm is SurrealDB-authored
    // and mirrored there in production.
    sqlx::query("INSERT INTO firm (id,name,slug,status) VALUES ($1,$2,$3,'active')")
        .bind(firm.id)
        .bind(&firm.name)
        .bind(format!("plans-{}", Uuid::new_v4().simple()))
        .execute(pg.pool())
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
        Some(json!({"plan_accepted_on": "2026-11-04"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted["plan_case_number"], "4900000-001");
    assert_eq!(accepted["plan_state"], "wa");
    assert_eq!(accepted["plan_accepted_on"], "2026-11-04");
    let (status, read) = send(&app, &owner, "GET", &link, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read, accepted);

    // A plan the Firm does not hold, and a malformed number, are refused.
    assert_eq!(
        send(
            &app,
            &owner,
            "PATCH",
            &link,
            Some(json!({"legal_plan": "Elsewhere"}))
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
}
