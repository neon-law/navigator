#![allow(clippy::doc_markdown)]
//! Integration tests for the matter's stage doors: the stage chip's read and
//! the set command beside the project-costs and project-task APIs.
//!
//! The stage store is PostgreSQL-native, so these tests run on the PostgreSQL
//! lane (`NAVIGATOR_TEST_POSTGRES_PORT`, the store crate's own fixture
//! contract): each test process builds one canonical-schema database, points
//! the app's shared `PostgresDb::from_env()` pool at it, and drives the real
//! handlers through the full router — embedded Rego policy included. Without
//! the lane they skip, unless `NAV_REQUIRE_POSTGRES=1` turns the skip into a
//! failure.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use serde_json::json;
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "api-project-stages-test-key";

static DATABASE: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

/// Create one canonical-schema database on the lane and point the app's
/// `PostgresDb::from_env()` pool at it. `None` when the lane is absent —
/// the test then skips.
async fn stage_store() -> Option<&'static str> {
    DATABASE
        .get_or_init(|| async {
            let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
                assert_ne!(
                    std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                    Ok("1"),
                    "the PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
                );
                return None;
            };
            let admin_database = std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                .unwrap_or_else(|_| "smoke".into());
            let options = sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port.parse().expect("the lane port is a number"))
                .username("postgres")
                .password("synthetic")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable);
            let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
                .await
                .unwrap();
            let database = format!("stage_api_test_{}", Uuid::now_v7().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let setup = sqlx::PgPool::connect_with(options.database(&database))
                .await
                .unwrap();
            // The graph migration needs the AGE extension, which only a
            // superuser creates: the lane's superuser init path stands in
            // for the application's `roles.sql`.
            sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
                .execute(&setup)
                .await
                .unwrap();
            sqlx::raw_sql(store::postgres_schema::canonical_schema())
                .execute(&setup)
                .await
                .unwrap();
            setup.close().await;
            std::env::set_var("NAVIGATOR_POSTGRES_HOST", "127.0.0.1");
            std::env::set_var("NAVIGATOR_POSTGRES_PORT", &port);
            std::env::set_var("NAVIGATOR_POSTGRES_USER", "postgres");
            std::env::set_var("NAVIGATOR_POSTGRES_PASSWORD", "synthetic");
            std::env::set_var("NAVIGATOR_POSTGRES_DATABASE", &database);
            std::env::set_var("NAVIGATOR_POSTGRES_SSLMODE", "disable");
            Some(database)
        })
        .await
        .as_deref()
}

fn bearer(person_id: Uuid, role: Role) -> String {
    let mut s = SessionData::fresh("api-stages-sub", role);
    s.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

struct Fixture {
    app: axum::Router,
    firm_id: Uuid,
    project_id: Uuid,
    other_project_id: Uuid,
    no_firm_project_id: Uuid,
    lawyer: String,
    client: String,
    outsider: String,
    clerk: String,
}

/// A Firm with real identity rows behind it — the stage vocabulary hangs off
/// the Firm, and PostgreSQL validates every reference.
async fn seed_firm(surreal: &store::surreal::SurrealDb, name: &str) -> Uuid {
    let run = Uuid::now_v7().simple();
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Stage Jurisdiction {name}"),
            format!("sj-{run}"),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(surreal, &format!("Stage Entity Type {name}"))
        .await
        .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: format!("Stage Firm Entity {name}"),
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
    let dri = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(
            name,
            format!("firm-dri-{run}@example.com"),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        surreal,
        &store::firms::NewFirm {
            name: name.to_string(),
            status: "active".into(),
            entity_id: entity.id,
            admin_dri_person_id: dri.id,
        },
    )
    .await
    .unwrap();
    firm.id
}

async fn seed_matter(
    surreal: &store::surreal::SurrealDb,
    code: &str,
    firm_id: Option<Uuid>,
) -> Uuid {
    let run = Uuid::now_v7().simple();
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Test Jurisdiction {code}"),
            format!("tj-{run}"),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(surreal, &format!("Test Entity Type {code}"))
        .await
        .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: format!("Matter Entity {code}"),
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
            code: code.to_string(),
            name: "Matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            firm_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

/// The Firm's vocabulary, authored directly in the stage store.
async fn seed_definitions(firm_id: Uuid) -> (Uuid, Uuid) {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    let intake = store::case_stages::create_definition(
        &pg,
        firm_id,
        &store::case_stages::NewStageDefinition {
            label: "Intake".into(),
            position: 0,
            case_type: None,
        },
    )
    .await
    .unwrap();
    let discovery = store::case_stages::create_definition(
        &pg,
        firm_id,
        &store::case_stages::NewStageDefinition {
            label: "Discovery".into(),
            position: 1,
            case_type: None,
        },
    )
    .await
    .unwrap();
    (intake.id, discovery.id)
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-stages-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let firm_id = seed_firm(&surreal, "Stage Test Firm").await;
    let project_id = seed_matter(
        &surreal,
        &format!("matter-{}", Uuid::now_v7()),
        Some(firm_id),
    )
    .await;
    let other_project_id = seed_matter(
        &surreal,
        &format!("other-{}", Uuid::now_v7()),
        Some(firm_id),
    )
    .await;
    let no_firm_project_id =
        seed_matter(&surreal, &format!("nofirm-{}", Uuid::now_v7()), None).await;

    let run = Uuid::now_v7().simple();
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            format!("lawyer-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(&surreal, other_project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(&surreal, no_firm_project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Client",
            format!("client-{run}@example.com"),
            Role::Client,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, client.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Outsider",
            format!("outsider-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let clerk = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Clerk",
            format!("clerk-{run}@example.com"),
            Role::Clerk,
        ),
    )
    .await
    .unwrap();

    let state = AppState {
        sessions: SessionStore::new(KEY),
        storage,
        ..portal::test_support::app_state(surreal.clone()).await
    };
    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        firm_id,
        project_id,
        other_project_id,
        no_firm_project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        client: bearer(client.id, Role::Client),
        outsider: bearer(outsider.id, Role::Lawyer),
        clerk: bearer(clerk.id, Role::Clerk),
    }
}

async fn get(fx: &Fixture, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn post(
    fx: &Fixture,
    path: &str,
    auth: Option<&str>,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn stages_path(project_id: Uuid) -> String {
    format!("/app/api/projects/{project_id}/stages")
}

async fn set_stage(
    fx: &Fixture,
    project_id: Uuid,
    auth: &str,
    stage_id: Uuid,
    note: Option<&str>,
) -> axum::http::Response<Body> {
    post(
        fx,
        &format!("{}/set", stages_path(project_id)),
        Some(auth),
        json!({"stage_id": stage_id.to_string(), "note": note}),
    )
    .await
}

#[tokio::test]
async fn the_stage_chip_round_trips_a_transition() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (intake, discovery) = seed_definitions(fx.firm_id).await;
    let path = stages_path(fx.project_id);

    // Before the first transition: the Firm's vocabulary is present, ordered,
    // and the matter holds no stage.
    let body = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
    let definitions = body["definitions"].as_array().expect("the vocabulary");
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0]["label"], "Intake");
    assert_eq!(definitions[1]["label"], "Discovery");
    assert_eq!(body["current"], serde_json::Value::Null);
    assert_eq!(body["history"].as_array().unwrap().len(), 0);

    // Set the first stage: the event has no origin, and the chip reflects it.
    let resp = set_stage(&fx, fx.project_id, &fx.lawyer, discovery, None).await;
    assert_eq!(resp.status(), StatusCode::OK, "the first transition lands");
    let event = body_json(resp).await;
    assert_eq!(event["from_stage_id"], serde_json::Value::Null);
    assert_eq!(
        event["to_stage_id"].as_str(),
        Some(discovery.to_string().as_str())
    );

    let body = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
    assert_eq!(
        body["current"]["id"].as_str(),
        Some(discovery.to_string().as_str())
    );
    assert_eq!(body["history"].as_array().unwrap().len(), 1);

    // Stages may move backward — a matter can return to discovery — because
    // the Firm's list orders the workflow, not the calendar.
    let resp = set_stage(
        &fx,
        fx.project_id,
        &fx.lawyer,
        intake,
        Some("Back to intake"),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let event = body_json(resp).await;
    assert_eq!(
        event["from_stage_id"].as_str(),
        Some(discovery.to_string().as_str()),
        "the transition continues the chain"
    );
    assert_eq!(event["note"].as_str(), Some("Back to intake"));

    let body = body_json(get(&fx, &path, Some(&fx.lawyer)).await).await;
    let history = body["history"].as_array().unwrap();
    assert_eq!(history.len(), 2, "the history is append-only");
    assert_eq!(
        history[0]["to_stage_id"].as_str(),
        Some(discovery.to_string().as_str())
    );
    assert_eq!(
        body["current"]["id"].as_str(),
        Some(intake.to_string().as_str())
    );
}

#[tokio::test]
async fn setting_the_current_stage_again_is_a_no_op() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (intake, _) = seed_definitions(fx.firm_id).await;

    let first = body_json(set_stage(&fx, fx.project_id, &fx.lawyer, intake, None).await).await;
    let second = body_json(set_stage(&fx, fx.project_id, &fx.lawyer, intake, None).await).await;

    assert_eq!(
        first["id"], second["id"],
        "repeating the stage records no new event"
    );
    let body = body_json(get(&fx, &stages_path(fx.project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(body["history"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unknown_stage_is_refused() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (_intake, _discovery) = seed_definitions(fx.firm_id).await;

    let resp = set_stage(&fx, fx.project_id, &fx.lawyer, Uuid::nil(), None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND, "no such stage");

    // And the matter still holds no stage.
    let body = body_json(get(&fx, &stages_path(fx.project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(body["current"], serde_json::Value::Null);
}

#[tokio::test]
async fn a_stage_from_another_firms_vocabulary_is_refused() {
    let Some(_) = stage_store().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let other_firm = seed_firm(&surreal, "Other Stage Firm").await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    store::postgres_mirror::mirror(
        &surreal,
        &pg,
        &store::postgres_mirror::MirrorKey::Firm(other_firm),
    )
    .await
    .unwrap();
    let foreign = store::case_stages::create_definition(
        &pg,
        other_firm,
        &store::case_stages::NewStageDefinition {
            label: "Foreign stage".into(),
            position: 0,
            case_type: None,
        },
    )
    .await
    .unwrap();

    let fx = build_fixture().await;
    let (_intake, _discovery) = seed_definitions(fx.firm_id).await;

    let resp = set_stage(&fx, fx.project_id, &fx.lawyer, foreign.id, None).await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "the stage is not in the matter Firm's own vocabulary"
    );

    // And the matter still holds no stage.
    let body = body_json(get(&fx, &stages_path(fx.project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(body["current"], serde_json::Value::Null);
}

#[tokio::test]
async fn an_invalid_note_is_refused() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (_intake, discovery) = seed_definitions(fx.firm_id).await;

    let resp = set_stage(
        &fx,
        fx.project_id,
        &fx.lawyer,
        discovery,
        Some(&"x".repeat(501)),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // And nothing was recorded.
    let body = body_json(get(&fx, &stages_path(fx.project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(body["current"], serde_json::Value::Null);
}

#[tokio::test]
async fn a_matter_without_a_firm_has_no_vocabulary_and_no_stage() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (intake, _discovery) = seed_definitions(fx.firm_id).await;

    // The read: an empty vocabulary and no stage — the panel explains itself.
    let body =
        body_json(get(&fx, &stages_path(fx.no_firm_project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(body["definitions"].as_array().unwrap().len(), 0);
    assert_eq!(body["current"], serde_json::Value::Null);

    // The command: the store refuses a transition that names no Firm.
    let resp = set_stage(&fx, fx.no_firm_project_id, &fx.lawyer, intake, None).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_lawyer_off_the_matter_is_hidden() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (intake, _) = seed_definitions(fx.firm_id).await;
    let path = stages_path(fx.project_id);

    // A lawyer without a participation row: 404, the matter does not exist
    // for them — never a 403 naming it.
    assert_eq!(
        get(&fx, &path, Some(&fx.outsider)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        set_stage(&fx, fx.project_id, &fx.outsider, intake, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // A client-side participant is not lawyer tier: route admission refuses
    // before the matter question is ever asked.
    assert_eq!(
        get(&fx, &path, Some(&fx.client)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        set_stage(&fx, fx.project_id, &fx.client, intake, None)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // A clerk is the same refusal.
    assert_eq!(
        get(&fx, &path, Some(&fx.clerk)).await.status(),
        StatusCode::FORBIDDEN
    );
    // No session at all: unauthorized.
    assert_eq!(
        get(&fx, &path, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        post(
            &fx,
            &format!("{path}/set"),
            None,
            json!({"stage_id": intake.to_string()})
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stages_are_scoped_to_their_matter() {
    let Some(_) = stage_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let (_intake, discovery) = seed_definitions(fx.firm_id).await;

    // The lawyer is on both matters. Moving the second matter's stage never
    // touches the first's history.
    let resp = set_stage(&fx, fx.other_project_id, &fx.lawyer, discovery, None).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let first = body_json(get(&fx, &stages_path(fx.project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(
        first["current"],
        serde_json::Value::Null,
        "the first matter's stage is untouched"
    );
    assert_eq!(first["history"].as_array().unwrap().len(), 0);

    let second =
        body_json(get(&fx, &stages_path(fx.other_project_id), Some(&fx.lawyer)).await).await;
    assert_eq!(
        second["current"]["id"].as_str(),
        Some(discovery.to_string().as_str())
    );
}
