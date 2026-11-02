//! The parked-calls API door: a lawyer sees only their Firms' unattributed
//! calls, a stranger sees an empty queue, and a non-lawyer is refused before
//! the PostgreSQL lane is ever opened.
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use portal::api::ApiState;
use serde_json::Value;
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
    lawyer: portal::SessionData,
    outsider: portal::SessionData,
    client: portal::SessionData,
    firm_a: Uuid,
    firm_b: Uuid,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let entity_a = store::test_support::seed_entity(&surreal).await;
    let entity_b = store::test_support::seed_entity(&surreal).await;
    let dri_a = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Firm A administrator",
            "parked-admin-a@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let dri_b = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Firm B administrator",
            "parked-admin-b@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm_a = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: format!("Synthetic Firm A {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity_a,
            admin_dri_person_id: dri_a.id,
        },
    )
    .await
    .unwrap();
    let firm_b = store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: format!("Synthetic Firm B {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity_b,
            admin_dri_person_id: dri_b.id,
        },
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Firm A Lawyer", "parked-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::firms::add_membership(
        &surreal,
        &store::firms::NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id: firm_a.id,
            membership: store::firms::FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Unaffiliated Lawyer",
            "parked-outsider@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let client = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Client",
            "parked-client@example.com",
            Role::Client,
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
    let session = |email: &str, role: Role, person_id: Uuid| portal::SessionData {
        person_id: Some(person_id),
        ..portal::SessionData::fresh(email, role)
    };
    Fixture {
        app,
        lawyer: session("parked-lawyer@example.com", Role::Lawyer, lawyer.id),
        outsider: session("parked-outsider@example.com", Role::Lawyer, outsider.id),
        client: session("parked-client@example.com", Role::Client, client.id),
        firm_a: firm_a.id,
        firm_b: firm_b.id,
    }
}

#[tokio::test]
async fn parked_calls_lists_only_the_actors_firms_rows() {
    if std::env::var("NAV_PARKED_CALLS_CHILD").as_deref() == Ok("1") {
        run_parked_calls().await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "parked_calls_lists_only_the_actors_firms_rows",
            "--nocapture",
        ])
        .env("NAV_PARKED_CALLS_CHILD", "1")
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

async fn run_parked_calls() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let parked_row = |firm_id, sid: &str| store::billed_calls::NewBilledCall {
        call_sid: sid.into(),
        conversation_id: Uuid::now_v7(),
        firm_id,
        duration_seconds: 361,
        call_started_at: chrono::Utc::now(),
    };
    let firm_a_row =
        store::billed_calls::record(&pg, &parked_row(fixture.firm_a, "CAparked-firm-a"))
            .await
            .unwrap();
    store::billed_calls::record(&pg, &parked_row(fixture.firm_b, "CAparked-firm-b"))
        .await
        .unwrap();

    let response = request(&fixture.app, &fixture.lawyer).await;
    assert_eq!(response.0, StatusCode::OK);
    let rows = response.1.as_array().expect("a JSON array of parked calls");
    assert_eq!(
        rows.len(),
        1,
        "the lawyer sees only their Firm's parked calls, got {rows:?}"
    );
    assert_eq!(rows[0]["call_sid"], firm_a_row.call_sid);
    assert_eq!(rows[0]["firm_id"], firm_a_row.firm_id.to_string());
    assert_eq!(rows[0]["project_id"], Value::Null);
    assert_eq!(rows[0]["tenths"], 2, "6:01 bills two tenths");

    let stranger = request(&fixture.app, &fixture.outsider).await;
    assert_eq!(stranger.0, StatusCode::OK);
    assert!(
        stranger.1.as_array().unwrap().is_empty(),
        "a lawyer with no memberships sees an empty queue"
    );

    let refused = request(&fixture.app, &fixture.client).await;
    assert_eq!(
        refused.0,
        StatusCode::FORBIDDEN,
        "a non-lawyer is refused before the billing lane opens"
    );
}

type JsonResponse = (StatusCode, Value);

async fn request(app: &Router, session: &portal::SessionData) -> JsonResponse {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/app/api/inbox/parked-calls")
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap_or(Value::Null);
    (status, body)
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
    let database = format!("parked_calls_{}", Uuid::new_v4().simple());
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
