//! Handoff doors: a lawyer prepares, the recipient reads and downloads, anyone
//! else gets 404.

use std::sync::Arc;

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

async fn participant(
    surreal: &store::surreal::SurrealDb,
    project_id: Option<Uuid>,
    name: &str,
    role: Role,
) -> portal::SessionData {
    let email = format!(
        "{}@example.com",
        name.to_ascii_lowercase().replace(' ', ".")
    );
    let person = store::persons::create(surreal, &NewPerson::with_role(name, &email, role))
        .await
        .unwrap();
    if let Some(project_id) = project_id {
        store::participation::add_participant(
            surreal,
            &store::participation::AddParticipantCommand {
                project_id,
                person_id: person.id,
                dri: store::participation::DriRequest::Unchanged,
                actor: store::participation::DriActor::System,
            },
        )
        .await
        .unwrap();
    }
    portal::SessionData {
        person_id: Some(person.id),
        ..portal::SessionData::fresh(&email, role)
    }
}

async fn send(
    app: &Router,
    method: &'static str,
    uri: String,
    session: &portal::SessionData,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>, Option<String>) {
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
    let digest = response
        .headers()
        .get("x-manifest-sha256")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes, digest)
}

#[tokio::test]
async fn the_recipient_and_lawyers_reach_a_handoff_and_nobody_else_does() {
    if std::env::var("NAV_HANDOFF_API_CHILD").as_deref() == Ok("1") {
        run_lifecycle().await;
        return;
    }
    let Some((port, database)) = isolated_database().await else {
        return;
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_recipient_and_lawyers_reach_a_handoff_and_nobody_else_does",
            "--nocapture",
        ])
        .env("NAV_HANDOFF_API_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_STORAGE_BACKEND")
        .status()
        .unwrap();
    remove_isolated_database(&port, &database).await;
    assert!(status.success());
}

struct Fixture {
    app: Router,
    state: portal::AppState,
    project_id: Uuid,
    lawyer: portal::SessionData,
    client: portal::SessionData,
    other: portal::SessionData,
    outsider: portal::SessionData,
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("handoff-door-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Handoff Door Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = participant(&surreal, Some(project.id), "Door Lawyer", Role::Lawyer).await;
    let client = participant(&surreal, Some(project.id), "Door Client", Role::Client).await;
    let other = participant(&surreal, Some(project.id), "Other Client", Role::Client).await;
    let outsider = participant(&surreal, None, "Outside Lawyer", Role::Lawyer).await;
    let state = portal::test_support::app_state(surreal).await;
    store::documents::ingest_bytes_as(
        &state.surreal,
        &state.storage,
        &store::documents::IngestArgs {
            project_id: project.id,
            source: store::documents::source::UPLOAD,
            filename: "order.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::CLIENT,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity::default(),
        b"synthetic order",
    )
    .await
    .unwrap();
    let app = portal::api::routes().with_state(ApiState {
        surreal: state.surreal.clone(),
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: Arc::clone(&state.storage),
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
        state,
        project_id: project.id,
        lawyer,
        client,
        other,
        outsider,
    }
}

fn error_code(bytes: &[u8]) -> Value {
    serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null)["error"].clone()
}

async fn run_lifecycle() {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let fixture = fixture().await;
    let (manifest_uri, manifest_sha256) = prepare_and_read(&fixture).await;
    acknowledgments(&fixture, &manifest_uri, &manifest_sha256).await;
    refusals_after_tampering(&fixture, &manifest_uri).await;
    firmless_refusal(&fixture).await;
}

/// Prepare refusals, then one handoff its readers reach; returns the
/// manifest URI and digest.
async fn prepare_and_read(fixture: &Fixture) -> (String, String) {
    let Fixture {
        app,
        project_id,
        lawyer,
        client,
        other,
        outsider,
        ..
    } = fixture;
    let base = format!("/app/api/projects/{project_id}/handoffs");
    let body = json!({ "recipient_person_id": client.person_id, "required": true });
    let (status, _, _) = send(app, "POST", base.clone(), outsider, Some(body.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let personless = portal::SessionData {
        person_id: None,
        ..lawyer.clone()
    };
    let (status, _, _) = send(app, "POST", base.clone(), &personless, Some(body.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a session with no Person");
    let (status, refused, _) = send(
        app,
        "POST",
        base.clone(),
        lawyer,
        Some(json!({ "recipient_person_id": outsider.person_id })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&refused), "recipient_not_participant");
    let (status, refused, _) = send(
        app,
        "POST",
        base.clone(),
        lawyer,
        Some(json!({ "recipient_person_id": Uuid::now_v7() })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(&refused), "recipient_not_participant");
    let (status, created, _) = send(app, "POST", base.clone(), lawyer, Some(body)).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&created)
    );
    let created: Value = serde_json::from_slice(&created).unwrap();
    let handoff_id = created["handoff"]["id"].as_str().unwrap().to_string();
    let manifest_sha256 = created["handoff"]["manifest_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created["manifest"]["files"][0]["path"], "order.pdf");

    let manifest_uri = format!("{base}/{handoff_id}");
    for (session, expected) in [
        (&client, StatusCode::OK),
        (&lawyer, StatusCode::OK),
        (&other, StatusCode::NOT_FOUND),
        (&outsider, StatusCode::NOT_FOUND),
    ] {
        let (status, _, _) = send(app, "GET", manifest_uri.clone(), session, None).await;
        assert_eq!(status, expected);
    }
    let (status, read, _) = send(app, "GET", manifest_uri.clone(), client, None).await;
    assert_eq!(status, StatusCode::OK);
    let read: Value = serde_json::from_slice(&read).unwrap();
    assert_eq!(read["handoff"]["manifest_sha256"], manifest_sha256.as_str());
    assert_eq!(read["manifest"]["files"][0]["path"], "order.pdf");
    let unknown = format!("{base}/{}", Uuid::now_v7());
    let (status, _, _) = send(app, "GET", unknown.clone(), lawyer, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown handoff");
    let (status, _, _) = send(app, "GET", format!("{unknown}/bundle"), lawyer, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown handoff's bundle");
    let foreign = format!("/app/api/projects/{}/handoffs/{handoff_id}", Uuid::now_v7());
    let (status, _, _) = send(app, "GET", foreign, lawyer, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the handoff under another Project"
    );

    let (status, _, _) = send(app, "GET", format!("{manifest_uri}/bundle"), other, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, bundle, digest) =
        send(app, "GET", format!("{manifest_uri}/bundle"), client, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(digest.as_deref(), Some(manifest_sha256.as_str()));
    assert!(bundle.starts_with(b"PK"));
    let (status, bundle, _) =
        send(app, "GET", format!("{manifest_uri}/bundle"), lawyer, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(bundle.starts_with(b"PK"));
    (manifest_uri, manifest_sha256)
}

async fn acknowledgments(fixture: &Fixture, manifest_uri: &str, manifest_sha256: &str) {
    let Fixture {
        app,
        project_id,
        lawyer,
        client,
        other,
        ..
    } = fixture;
    let unknown = format!("/app/api/projects/{project_id}/handoffs/{}", Uuid::now_v7());
    let handoff_id = manifest_uri.rsplit('/').next().unwrap();
    // Acknowledgment: only the recipient, only the exact manifest.
    let acknowledge = format!("{manifest_uri}/acknowledge");
    let exact = json!({ "manifest_sha256": manifest_sha256 });
    for session in [&other, &lawyer] {
        let (status, _, _) = send(
            app,
            "POST",
            acknowledge.clone(),
            session,
            Some(exact.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "only the recipient acknowledges"
        );
    }
    let (status, _, _) = send(
        app,
        "POST",
        format!("{unknown}/acknowledge"),
        client,
        Some(exact.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "an unknown handoff");
    let client_without_person = portal::SessionData {
        person_id: None,
        ..client.clone()
    };
    let (status, _, _) = send(
        app,
        "POST",
        acknowledge.clone(),
        &client_without_person,
        Some(exact.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, mismatch, _) = send(
        app,
        "POST",
        acknowledge.clone(),
        client,
        Some(json!({ "manifest_sha256": "0".repeat(64) })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&mismatch), "manifest_mismatch");
    let (status, acknowledged, _) = send(
        app,
        "POST",
        acknowledge.clone(),
        client,
        Some(exact.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&acknowledged)
    );
    let acknowledged: Value = serde_json::from_slice(&acknowledged).unwrap();
    assert_eq!(acknowledged["handoff_id"], handoff_id);
    assert_eq!(acknowledged["manifest_sha256"], manifest_sha256);
    assert_eq!(
        acknowledged["acknowledged_by_person_id"],
        json!(client.person_id)
    );
    let (status, _, _) = send(app, "POST", acknowledge, client, Some(exact)).await;
    assert_eq!(status, StatusCode::OK, "acknowledging again is idempotent");
}

/// Bytes that no longer match the manifest refuse the bundle and a new
/// handoff alike.
async fn refusals_after_tampering(fixture: &Fixture, manifest_uri: &str) {
    let Fixture {
        app,
        state,
        project_id,
        lawyer,
        client,
        ..
    } = fixture;
    let base = format!("/app/api/projects/{project_id}/handoffs");
    let asset = store::assets::for_project(&state.surreal, *project_id)
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    state
        .storage
        .put(&asset.storage_key, b"tampered bytes", "application/pdf")
        .await
        .unwrap();
    let (status, refused, _) =
        send(app, "GET", format!("{manifest_uri}/bundle"), client, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&refused), "handoff_files_unverifiable");
    let (status, refused, _) = send(
        app,
        "POST",
        base,
        lawyer,
        Some(json!({ "recipient_person_id": client.person_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&refused), "handoff_files_unverifiable");
}

/// A Project with no Firm has no handoff.
async fn firmless_refusal(fixture: &Fixture) {
    let Fixture {
        app,
        state,
        lawyer,
        client,
        ..
    } = fixture;
    let firmless = store::projects::create(
        &state.surreal,
        &store::projects::NewProject {
            code: format!(
                "handoff-nofirm-{}",
                &Uuid::new_v4().simple().to_string()[..8]
            ),
            name: "Synthetic Firmless Handoff Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&state.surreal).await,
            firm_id: None,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    for person in [lawyer.person_id, client.person_id] {
        store::participation::add_participant(
            &state.surreal,
            &store::participation::AddParticipantCommand {
                project_id: firmless.id,
                person_id: person.unwrap(),
                dri: store::participation::DriRequest::Unchanged,
                actor: store::participation::DriActor::System,
            },
        )
        .await
        .unwrap();
    }
    let (status, refused, _) = send(
        app,
        "POST",
        format!("/app/api/projects/{}/handoffs", firmless.id),
        lawyer,
        Some(json!({ "recipient_person_id": client.person_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error_code(&refused), "project_has_no_firm");
}

#[tokio::test]
async fn every_handoff_door_fails_closed_without_postgres() {
    if std::env::var("NAV_HANDOFF_API_NO_PG_CHILD").as_deref() == Ok("1") {
        run_without_postgres().await;
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "every_handoff_door_fails_closed_without_postgres",
            "--nocapture",
        ])
        .env("NAV_HANDOFF_API_NO_PG_CHILD", "1")
        .env_remove("NAVIGATOR_POSTGRES_HOST")
        .env_remove("NAVIGATOR_POSTGRES_PORT")
        .env_remove("NAVIGATOR_POSTGRES_DATABASE")
        .env_remove("NAVIGATOR_STORAGE_BACKEND")
        .status()
        .unwrap();
    assert!(status.success());
}

async fn run_without_postgres() {
    let Fixture {
        app,
        project_id,
        lawyer,
        client,
        ..
    } = fixture().await;
    let base = format!("/app/api/projects/{project_id}/handoffs");
    let handoff = format!("{base}/{}", Uuid::now_v7());
    let (status, body, _) = send(
        &app,
        "POST",
        base.clone(),
        &lawyer,
        Some(json!({ "recipient_person_id": client.person_id })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&body), "handoff_store_unavailable");
    for uri in [handoff.clone(), format!("{handoff}/bundle")] {
        let (status, body, _) = send(&app, "GET", uri, &client, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error_code(&body), "handoff_store_unavailable");
    }
    let (status, body, _) = send(
        &app,
        "POST",
        format!("{handoff}/acknowledge"),
        &client,
        Some(json!({ "manifest_sha256": "0".repeat(64) })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(&body), "handoff_store_unavailable");

    // The tier gates answer before any store opens.
    let (status, _, _) = send(
        &app,
        "POST",
        base.clone(),
        &client,
        Some(json!({ "recipient_person_id": client.person_id })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a client never prepares");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(handoff)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
    let database = format!("handoff_api_{}", Uuid::new_v4().simple());
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
