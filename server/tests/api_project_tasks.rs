#![allow(clippy::doc_markdown)]
//! Integration tests for the matter-task doors: the Tasks tab's write surface
//! beside the project-costs API — create, assign, and transition, plus the
//! list read.
//!
//! The task store is PostgreSQL-native, so these tests run on the PostgreSQL
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

const KEY: &str = "api-project-tasks-test-key";

static DATABASE: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

/// Create one canonical-schema database on the lane and point the app's
/// `PostgresDb::from_env()` pool at it. `None` when the lane is absent —
/// the test then skips.
async fn task_store() -> Option<&'static str> {
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
            let database = format!("task_api_test_{}", Uuid::now_v7().simple());
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
    let mut s = SessionData::fresh("api-tasks-sub", role);
    s.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

struct Fixture {
    app: axum::Router,
    project_id: Uuid,
    other_project_id: Uuid,
    lawyer: String,
    lawyer_id: Uuid,
    client: String,
    outsider: String,
    clerk: String,
}

async fn seed_matter(surreal: &store::surreal::SurrealDb, code: &str) -> Uuid {
    // `seed_entity` deliberately points its `entity_type_id` and
    // `jurisdiction_id` at rows that exist nowhere: SurrealDB does not
    // validate a link. PostgreSQL does, so the pre-command mirror would
    // refuse the matter's entity — finding no source row for either
    // reference — and every task door would answer 503. The fixture
    // therefore resolves real identity rows, exactly as a production
    // matter carries.
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Test Jurisdiction {code}"),
            format!("tj-{}", Uuid::now_v7().simple()),
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
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-tasks-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project_id = seed_matter(&surreal, &format!("matter-{}", Uuid::now_v7())).await;
    let other_project_id = seed_matter(&surreal, &format!("other-{}", Uuid::now_v7())).await;
    let run = Uuid::now_v7().simple();
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            &format!("lawyer-{run}@example.com"),
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
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Client",
            &format!("client-{run}@example.com"),
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
            &format!("outsider-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let clerk = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Clerk",
            &format!("clerk-{run}@example.com"),
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
        project_id,
        other_project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        lawyer_id: lawyer.id,
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

/// Create one task, returning its id.
async fn create_task(fx: &Fixture) -> Uuid {
    let resp = post(
        fx,
        &format!("/app/api/projects/{}/tasks", fx.project_id),
        Some(&fx.lawyer),
        json!({"title": "Serve discovery responses", "due_on": "2026-12-01"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "the task opens");
    let body = body_json(resp).await;
    body["id"]
        .as_str()
        .expect("the row carries its id")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn the_tasks_tab_round_trips_a_task() {
    let Some(_) = task_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let list_path = format!("/app/api/projects/{}/tasks", fx.project_id);

    // Create: opens in `todo`.
    let resp = post(
        &fx,
        &list_path,
        Some(&fx.lawyer),
        json!({"title": "Serve discovery responses"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let task = body_json(resp).await;
    assert_eq!(task["state"], "todo", "a new task opens in `todo`");
    let task_id: Uuid = task["id"].as_str().unwrap().parse().unwrap();

    // Assign: a participant on the same matter.
    let assign_path = format!("/app/api/projects/{}/tasks/{task_id}/assign", fx.project_id);
    let resp = post(
        &fx,
        &assign_path,
        Some(&fx.lawyer),
        json!({"assignee_id": fx.lawyer_id}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let task = body_json(resp).await;
    assert_eq!(
        task["assignee_id"].as_str(),
        Some(fx.lawyer_id.to_string().as_str()),
        "the assignee lands"
    );

    // Transition: one step at a time, ending in `done`.
    for word in ["in_progress", "done"] {
        let transition_path = format!(
            "/app/api/projects/{}/tasks/{task_id}/transition",
            fx.project_id
        );
        let resp = post(
            &fx,
            &transition_path,
            Some(&fx.lawyer),
            json!({"state": word}),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK, "the move to `{word}` lands");
        assert_eq!(body_json(resp).await["state"], word);
    }

    // The list read carries state and assignee for the Tasks tab.
    let tasks = body_json(get(&fx, &list_path, Some(&fx.lawyer)).await).await["tasks"]
        .as_array()
        .expect("the list wraps its rows")
        .clone();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["state"], "done");
    assert_eq!(
        tasks[0]["assignee_id"].as_str(),
        Some(fx.lawyer_id.to_string().as_str())
    );
}

#[tokio::test]
async fn an_invalid_transition_is_refused() {
    let Some(_) = task_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let task_id = create_task(&fx).await;

    // Done is reachable only through `in_progress` — one step at a time.
    let transition_path = format!(
        "/app/api/projects/{}/tasks/{task_id}/transition",
        fx.project_id
    );
    let resp = post(
        &fx,
        &transition_path,
        Some(&fx.lawyer),
        json!({"state": "done"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // An unknown state word is a refusal, never a store error.
    let resp = post(
        &fx,
        &transition_path,
        Some(&fx.lawyer),
        json!({"state": "archived"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // And the task never moved.
    let tasks = body_json(
        get(
            &fx,
            &format!("/app/api/projects/{}/tasks", fx.project_id),
            Some(&fx.lawyer),
        )
        .await,
    )
    .await["tasks"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tasks[0]["state"], "todo");
}

#[tokio::test]
async fn a_lawyer_off_the_matter_is_hidden() {
    let Some(_) = task_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let list_path = format!("/app/api/projects/{}/tasks", fx.project_id);

    // A lawyer without a participation row: 404, the matter does not exist
    // for them — never a 403 naming it.
    assert_eq!(
        post(
            &fx,
            &list_path,
            Some(&fx.outsider),
            json!({"title": "Sneak"})
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&fx, &list_path, Some(&fx.outsider)).await.status(),
        StatusCode::NOT_FOUND
    );

    // A client-side participant is not lawyer tier: route admission refuses
    // before the matter question is ever asked.
    assert_eq!(
        post(&fx, &list_path, Some(&fx.client), json!({"title": "Sneak"}))
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    // A clerk is the same refusal.
    assert_eq!(
        get(&fx, &list_path, Some(&fx.clerk)).await.status(),
        StatusCode::FORBIDDEN
    );
    // No session at all: unauthorized.
    assert_eq!(
        get(&fx, &list_path, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn a_task_of_another_matter_is_hidden() {
    let Some(_) = task_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let task_id = create_task(&fx).await;

    // The lawyer is on both matters, but the task belongs to the first one:
    // naming it under the second matter's URL is the non-disclosing 404, for
    // both commands.
    let assign_path = format!(
        "/app/api/projects/{}/tasks/{task_id}/assign",
        fx.other_project_id
    );
    let transition_path = format!(
        "/app/api/projects/{}/tasks/{task_id}/transition",
        fx.other_project_id
    );
    assert_eq!(
        post(
            &fx,
            &assign_path,
            Some(&fx.lawyer),
            json!({"assignee_id": fx.lawyer_id})
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(
            &fx,
            &transition_path,
            Some(&fx.lawyer),
            json!({"state": "in_progress"})
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    // And the task itself never moved.
    let tasks = body_json(
        get(
            &fx,
            &format!("/app/api/projects/{}/tasks", fx.project_id),
            Some(&fx.lawyer),
        )
        .await,
    )
    .await["tasks"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tasks[0]["state"], "todo");
    assert_eq!(tasks[0]["assignee_id"], serde_json::Value::Null);
}

#[tokio::test]
async fn clearing_an_assignee_and_repeating_it_settle() {
    let Some(_) = task_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let task_id = create_task(&fx).await;
    let assign_path = format!("/app/api/projects/{}/tasks/{task_id}/assign", fx.project_id);

    // Repeating the current assignee is a no-op, and `null` clears.
    let resp = post(
        &fx,
        &assign_path,
        Some(&fx.lawyer),
        json!({"assignee_id": fx.lawyer_id}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = post(
        &fx,
        &assign_path,
        Some(&fx.lawyer),
        json!({"assignee_id": fx.lawyer_id}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = post(
        &fx,
        &assign_path,
        Some(&fx.lawyer),
        json!({"assignee_id": null}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        body_json(resp).await["assignee_id"],
        serde_json::Value::Null
    );
}
