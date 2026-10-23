//! Source import doors validate before writing, require Forgejo, and stay
//! matter-scoped.

use std::io::{Cursor, Write as _};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use portal::api::ApiState;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::persons::{NewPerson, Role};
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand};
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    lawyer: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("import-door-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Import Door Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Door Lawyer", "door-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id: project.id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &NewPerson::with_role("Other Lawyer", "door-outsider@example.com", Role::Lawyer),
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
    let session = |person: &store::persons::Person| portal::SessionData {
        person_id: Some(person.id),
        ..portal::SessionData::fresh(&person.email, Role::Lawyer)
    };
    Fixture {
        app,
        surreal: state.surreal.clone(),
        project_id: project.id,
        lawyer: session(&lawyer),
        outsider: session(&outsider),
    }
}

fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, text) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(text.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

async fn send(
    app: &Router,
    method: &'static str,
    uri: String,
    session: &portal::SessionData,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/zip")
                .extension(session.clone())
                .body(Body::from(body))
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
async fn an_import_is_validated_recorded_once_and_scoped_to_the_matter() {
    if std::env::var("NAV_SOURCE_IMPORT_CHILD").as_deref() == Ok("1") {
        run_lifecycle().await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_import_is_validated_recorded_once_and_scoped_to_the_matter",
            "--nocapture",
        ])
        .env("NAV_SOURCE_IMPORT_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env("NAVIGATOR_FORGE_BACKEND", "forgejo")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

async fn run_lifecycle() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let uri = |key: &str| {
        format!(
            "/app/api/projects/{}/source-imports?operation_key={key}",
            fixture.project_id
        )
    };
    let hostile = archive(&[("matter/../../escape.md", "x")]);
    let (status, body) = send(
        &fixture.app,
        "POST",
        uri("door-0"),
        &fixture.lawyer,
        hostile,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM project_source_import")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(count, 0, "an invalid archive writes nothing");

    let source = archive(&[("navigator.yaml", "version: 1\n"), ("drafts/a.md", "a\n")]);
    let (status, _) = send(
        &fixture.app,
        "POST",
        uri("door-1"),
        &fixture.outsider,
        source.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, first) = send(
        &fixture.app,
        "POST",
        uri("door-1"),
        &fixture.lawyer,
        source.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["state"], "pending");
    let (status, replay) = send(
        &fixture.app,
        "POST",
        uri("door-1"),
        &fixture.lawyer,
        source.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(replay["id"], first["id"]);
    let (status, _) = send(&fixture.app, "POST", uri("door-2"), &fixture.lawyer, source).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "one pending import per Project"
    );

    let status_uri = format!(
        "/app/api/projects/{}/source-imports/{}",
        fixture.project_id,
        first["id"].as_str().unwrap()
    );
    let (status, read) = send(
        &fixture.app,
        "GET",
        status_uri.clone(),
        &fixture.lawyer,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["tree_sha256"], first["tree_sha256"]);
    let (status, _) = send(
        &fixture.app,
        "GET",
        status_uri,
        &fixture.outsider,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    refusals_after_one_import(&fixture).await;
}

/// Refusals once one import is pending: an unknown import, a reused key, a bad
/// key, a session with no Person, a firmless Project, and an initialized one.
async fn refusals_after_one_import(fixture: &Fixture) {
    let uri = |key: &str| {
        format!(
            "/app/api/projects/{}/source-imports?operation_key={key}",
            fixture.project_id
        )
    };
    let (status, _) = send(
        &fixture.app,
        "GET",
        format!(
            "/app/api/projects/{}/source-imports/{}",
            fixture.project_id,
            Uuid::now_v7()
        ),
        &fixture.lawyer,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown import");

    let other_tree = archive(&[("navigator.yaml", "version: 1\n"), ("drafts/b.md", "b\n")]);
    let (status, conflict) = send(
        &fixture.app,
        "POST",
        uri("door-1"),
        &fixture.lawyer,
        other_tree.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"], "operation_key_conflict");
    let (status, invalid) = send(
        &fixture.app,
        "POST",
        uri("door%20space"),
        &fixture.lawyer,
        other_tree.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid}");
    assert_eq!(invalid["error"], "invalid_source_archive");
    let personless = portal::SessionData {
        person_id: None,
        ..fixture.lawyer.clone()
    };
    let (status, _) = send(
        &fixture.app,
        "POST",
        uri("door-3"),
        &personless,
        other_tree.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a session with no Person");

    // A Project with no Firm has no repository to import into.
    let firmless = extra_project(fixture, None, None).await;
    let (status, refused) = send(
        &fixture.app,
        "POST",
        format!("/app/api/projects/{firmless}/source-imports?operation_key=door-4"),
        &fixture.lawyer,
        other_tree.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "project_has_no_firm");

    // A Project whose repository is already initialized is not re-seeded.
    let firm_id = store::projects::find_by_id(&fixture.surreal, fixture.project_id)
        .await
        .unwrap()
        .unwrap()
        .firm_id;
    let initialized = extra_project(fixture, firm_id, Some("2026-01-01T00:00:00Z")).await;
    let (status, refused) = send(
        &fixture.app,
        "POST",
        format!("/app/api/projects/{initialized}/source-imports?operation_key=door-5"),
        &fixture.lawyer,
        other_tree,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "source_already_initialized");
}

/// Another synthetic matter the fixture's lawyer works on.
async fn extra_project(
    fixture: &Fixture,
    firm_id: Option<Uuid>,
    git_initialized_at: Option<&str>,
) -> Uuid {
    let created = store::projects::create(
        &fixture.surreal,
        &store::projects::NewProject {
            code: format!("import-extra-{}", &Uuid::new_v4().simple().to_string()[..8]),
            name: "Synthetic Extra Import Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&fixture.surreal).await,
            firm_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let project_id = if let Some(at) = git_initialized_at {
        // Restoring a copy is the one public writer of `git_initialized_at`.
        let mut copy = created.clone();
        copy.id = Uuid::now_v7();
        copy.code = format!("{}-git", created.code);
        copy.git_initialized_at = Some(at.to_string());
        store::projects::restore_archived(&fixture.surreal, &copy)
            .await
            .unwrap();
        copy.id
    } else {
        created.id
    };
    store::participation::add_participant(
        &fixture.surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: fixture.lawyer.person_id.unwrap(),
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    project_id
}

#[tokio::test]
async fn an_import_refuses_without_forgejo_or_postgres() {
    if let Ok(mode) = std::env::var("NAV_SOURCE_IMPORT_UNAVAILABLE_CHILD") {
        run_unavailable(&mode).await;
        return;
    }
    for (mode, backend) in [
        ("github", Some("github")),
        ("default", None),
        ("misconfigured", Some("subversion")),
        ("no-postgres", Some("forgejo")),
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "an_import_refuses_without_forgejo_or_postgres",
                "--nocapture",
            ])
            .env("NAV_SOURCE_IMPORT_UNAVAILABLE_CHILD", mode)
            .env_remove("NAVIGATOR_POSTGRES_HOST")
            .env_remove("NAVIGATOR_POSTGRES_PORT")
            .env_remove("NAVIGATOR_POSTGRES_DATABASE");
        match backend {
            Some(backend) => child.env("NAVIGATOR_FORGE_BACKEND", backend),
            None => child.env_remove("NAVIGATOR_FORGE_BACKEND"),
        };
        assert!(child.status().unwrap().success(), "{mode}");
    }
}

async fn run_unavailable(mode: &str) {
    let fixture = fixture().await;
    let source = archive(&[("navigator.yaml", "version: 1\n")]);
    let request_uri = format!(
        "/app/api/projects/{}/source-imports?operation_key=door-unavailable",
        fixture.project_id
    );
    let (status, body) = send(&fixture.app, "POST", request_uri, &fixture.lawyer, source).await;
    let (expected_status, expected_error) = match mode {
        "github" | "default" => (StatusCode::CONFLICT, "source_import_requires_forgejo"),
        "misconfigured" => (
            StatusCode::SERVICE_UNAVAILABLE,
            "forge_backend_misconfigured",
        ),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "source_import_store_unavailable",
        ),
    };
    assert_eq!(status, expected_status, "{body}");
    assert_eq!(body["error"], expected_error);

    let status_uri = format!(
        "/app/api/projects/{}/source-imports/{}",
        fixture.project_id,
        Uuid::now_v7()
    );
    let (status, body) = send(
        &fixture.app,
        "GET",
        status_uri.clone(),
        &fixture.lawyer,
        Vec::new(),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], "source_import_store_unavailable");
    let (status, _) = send(
        &fixture.app,
        "GET",
        status_uri,
        &fixture.outsider,
        Vec::new(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the matter check comes before the store"
    );
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
    let database = format!("source_import_api_{}", Uuid::new_v4().simple());
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
