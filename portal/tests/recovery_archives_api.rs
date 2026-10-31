//! Recovery archive doors are admin-only, Firm-scoped, durable, and fail closed.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use portal::api::ApiState;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::persons::{NewPerson, Role};
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    state: portal::AppState,
    project_id: Uuid,
    firm_id: Uuid,
    owner: portal::SessionData,
    lawyer: portal::SessionData,
    foreign_admin: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!(
                "recovery-door-{}",
                &Uuid::now_v7().simple().to_string()[..8]
            ),
            name: "Synthetic Recovery Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let person = |name: &'static str, email: &'static str, role: Role| {
        let surreal = surreal.clone();
        async move {
            store::persons::create(&surreal, &NewPerson::with_role(name, email, role))
                .await
                .unwrap()
        }
    };
    let owner = person("Recovery Owner", "recovery-owner@example.com", Role::Owner).await;
    let lawyer = person(
        "Recovery Lawyer",
        "recovery-lawyer@example.com",
        Role::Lawyer,
    )
    .await;
    let foreign = person("Foreign Admin", "recovery-foreign@example.com", Role::Admin).await;
    store::firms::create(
        &surreal,
        &store::firms::NewFirm {
            name: "Foreign Synthetic Firm".into(),
            status: "active".into(),
            entity_id: seed_entity(&surreal).await,
            admin_dri_person_id: foreign.id,
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
    let session = |person: &store::persons::Person, role| portal::SessionData {
        person_id: Some(person.id),
        ..portal::SessionData::fresh(&person.email, role)
    };
    Fixture {
        app,
        project_id: project.id,
        firm_id,
        owner: session(&owner, Role::Owner),
        lawyer: session(&lawyer, Role::Lawyer),
        foreign_admin: session(&foreign, Role::Admin),
        state,
    }
}

async fn request(
    app: &Router,
    method: &'static str,
    uri: String,
    session: &portal::SessionData,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
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

#[tokio::test]
async fn a_lawyer_and_another_firms_admin_are_refused_before_any_store_opens() {
    let fixture = fixture().await;
    let base = format!("/app/api/projects/{}/recovery-archives", fixture.project_id);
    let (status, _) = request(&fixture.app, "GET", base.clone(), &fixture.lawyer, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(
        &fixture.app,
        "POST",
        base.clone(),
        &fixture.foreign_admin,
        Some(json!({ "operation_key": "door-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &fixture.app,
        "GET",
        format!("/app/api/projects/{}/recovery-archives", Uuid::now_v7()),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let personless = portal::SessionData {
        person_id: None,
        ..fixture.owner.clone()
    };
    let (status, _) = request(&fixture.app, "GET", base.clone(), &personless, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a session with no Person");
    let firmless = project(&fixture.state.surreal, None).await;
    let (status, _) = request(
        &fixture.app,
        "GET",
        format!("/app/api/projects/{firmless}/recovery-archives"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a Project with no Firm");
    let verify = format!("{base}/{}/verify", Uuid::now_v7());
    let (status, _) = request(&fixture.app, "POST", verify.clone(), &fixture.lawyer, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = request(&fixture.app, "POST", verify, &fixture.foreign_admin, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(base)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// One more synthetic matter, in `firm_id` or in no Firm at all.
async fn project(surreal: &store::surreal::SurrealDb, firm_id: Option<Uuid>) -> Uuid {
    store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: format!(
                "recovery-extra-{}",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            name: "Synthetic Extra Recovery Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(surreal).await,
            firm_id,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

#[tokio::test]
async fn every_recovery_door_fails_closed_without_postgres() {
    if std::env::var("NAV_RECOVERY_API_NO_PG_CHILD").as_deref() == Ok("1") {
        run_without_postgres().await;
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "every_recovery_door_fails_closed_without_postgres",
            "--nocapture",
        ])
        .env("NAV_RECOVERY_API_NO_PG_CHILD", "1")
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .env_remove("NAVIGATOR_POSTGRES_PORT")
        .env_remove("NAVIGATOR_POSTGRES_DATABASE")
        .status()
        .unwrap();
    assert!(status.success());
}

async fn run_without_postgres() {
    let fixture = fixture().await;
    let base = format!("/app/api/projects/{}/recovery-archives", fixture.project_id);
    for (method, uri, body) in [
        (
            "POST",
            base.clone(),
            Some(json!({ "operation_key": "door-pg" })),
        ),
        ("GET", base.clone(), None),
        ("POST", format!("{base}/{}/verify", Uuid::now_v7()), None),
    ] {
        let (status, body) = request(&fixture.app, method, uri, &fixture.owner, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "recovery_archive_store_unavailable");
    }
}

#[tokio::test]
async fn an_owner_requests_lists_and_verifies_a_durable_generation() {
    match std::env::var("NAV_RECOVERY_API_CHILD").as_deref() {
        Ok("1") => {
            run_lifecycle().await;
            return;
        }
        Ok("unconfigured") => {
            run_without_archive_lane().await;
            return;
        }
        _ => {}
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let archive_root = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_owner_requests_lists_and_verifies_a_durable_generation",
            "--nocapture",
        ])
        .env("NAV_RECOVERY_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env("NAVIGATOR_STORAGE_BACKEND", "fs")
        .env("NAVIGATOR_ARCHIVED_PROJECTS_FS_ROOT", archive_root.path())
        .env_remove("NAVIGATOR_STORAGE_FS_ROOT")
        .status()
        .unwrap();
    let unconfigured = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_owner_requests_lists_and_verifies_a_durable_generation",
            "--nocapture",
        ])
        .env("NAV_RECOVERY_API_CHILD", "unconfigured")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_STORAGE_BACKEND")
        .env_remove("NAVIGATOR_ARCHIVED_PROJECTS_FS_ROOT")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
    assert!(unconfigured.success());
}

/// PostgreSQL is reachable but the archived-projects lane is not configured:
/// requesting and verifying refuse, listing still answers.
async fn run_without_archive_lane() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let base = format!("/app/api/projects/{}/recovery-archives", fixture.project_id);
    for (uri, body) in [
        (base.clone(), Some(json!({ "operation_key": "door-lane" }))),
        (format!("{base}/{}/verify", Uuid::now_v7()), None),
    ] {
        let (status, body) = request(&fixture.app, "POST", uri, &fixture.owner, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "archived_projects_lane_unconfigured");
    }
    let (status, listed) = request(&fixture.app, "GET", base, &fixture.owner, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["generations"], json!([]));
}

async fn run_lifecycle() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let base = format!("/app/api/projects/{}/recovery-archives", fixture.project_id);
    let body = json!({ "operation_key": "door-close-1" });
    let (status, first) = request(
        &fixture.app,
        "POST",
        base.clone(),
        &fixture.owner,
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["state"], "pending");
    let (status, replay) = request(
        &fixture.app,
        "POST",
        base.clone(),
        &fixture.owner,
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["id"], first["id"]);
    let (status, invalid) = request(
        &fixture.app,
        "POST",
        base.clone(),
        &fixture.owner,
        Some(json!({ "operation_key": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid}");
    assert_eq!(invalid["error"], "invalid_archive_request");
    let generation_id = Uuid::parse_str(first["id"].as_str().unwrap()).unwrap();
    refusals_beside(&fixture, &base, generation_id).await;
    let verify = format!("{base}/{generation_id}/verify");
    let (status, uncommitted) =
        request(&fixture.app, "POST", verify.clone(), &fixture.owner, None).await;
    assert_eq!(uncommitted["error"], "archive_not_committed");
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an uncommitted generation does not verify"
    );

    // The ProjectRecovery schedule's unit of work, run directly.
    let archive = cloud::archived_projects_from_env().await.unwrap();
    let stores = store::project_archives::ArchiveStores {
        surreal: &fixture.state.surreal,
        pg: &pg,
        documents: &fixture.state.storage,
        archive: &archive,
        source_backups: None,
    };
    let committed =
        store::project_archives::resume_unattended(&stores, generation_id, chrono::Utc::now())
            .await
            .unwrap();
    assert_eq!(
        committed.state,
        store::project_archives::GenerationState::Committed
    );
    let (status, report) = request(&fixture.app, "POST", verify, &fixture.owner, None).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["failures"], json!([]));
    let (status, listed) = request(&fixture.app, "GET", base, &fixture.owner, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["generations"][0]["state"], "committed");
}

/// The refusals a pending generation sees: a key reused for a sibling Project,
/// a verify under the wrong Project, and an unknown generation.
async fn refusals_beside(fixture: &Fixture, base: &str, generation_id: Uuid) {
    let sibling = project(&fixture.state.surreal, Some(fixture.firm_id)).await;
    let sibling_base = format!("/app/api/projects/{sibling}/recovery-archives");
    let (status, conflict) = request(
        &fixture.app,
        "POST",
        sibling_base.clone(),
        &fixture.owner,
        Some(json!({ "operation_key": "door-close-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"], "operation_key_conflict");

    let (status, _) = request(
        &fixture.app,
        "POST",
        format!("{sibling_base}/{generation_id}/verify"),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a generation is verified only under its own Project"
    );
    let (status, _) = request(
        &fixture.app,
        "POST",
        format!("{base}/{}/verify", Uuid::now_v7()),
        &fixture.owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown generation");
}

async fn isolated_database() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return None;
    };
    let admin = PgPool::connect_with(admin_options(&port)).await.unwrap();
    let database = format!("recovery_api_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    Some((port, database))
}

fn admin_options(port: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable)
}

async fn remove_isolated_database(port: &str, database: &str) {
    let admin = PgPool::connect_with(admin_options(port)).await.unwrap();
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
