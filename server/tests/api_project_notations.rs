#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/notations` — the REST door
//! that opens a notation on an existing matter.
//!
//! This door funnels through the same `crate::project_notation::create_project_notation`
//! command the lawyer browser form drives (`POST /app/projects/:project_code/notations/new`,
//! covered in `project_notation_create.rs`), so these tests focus on the two
//! properties the REST adapter adds: the tier gate (LawyerSession → 401/403) and
//! the matter-scope gate (a lawyer who does not participate in the matter gets a
//! bare 404, admin bypasses), plus a live happy path proving the JSON contract.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::seed;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use workflows::{InMemoryRuntime, StateMachineRuntime};

/// A corpus template body — guaranteed to validate clean (CI checks it) and to
/// open a matter as its first notation (its kind opens a matter regardless of
/// the code it is committed under, exactly as `project_notation_create.rs` relies on).
const VALID_TEMPLATE: &str =
    include_str!("../../templates/notations/neon_law/onboarding__letter.md");

fn amendment_template() -> String {
    VALID_TEMPLATE.replace("code: onboarding__letter", "code: amendment")
}

/// Signing key shared by the app's `SessionStore` and the bearers the tests mint.
const KEY: &str = "api-project-notations-test-key";

// These tests mutate one process-wide repository root and invoke git in that
// shared directory. LLVM-instrumented macOS test processes can otherwise race
// over inherited file descriptors, surfacing an intermittent EBADF.
static REPO_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// One stable repo root for every test in this binary, so the command's
/// `RepoStore::from_env()` reads the same root regardless of which test set it
/// (each test isolates by its own unique `project_id` sub-repo).
fn repo_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join("navigator-api-project-notations-repos");
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("NAVIGATOR_GIT_REPO_ROOT", &root);
    root
}

async fn build_app() -> (axum::Router, store::surreal::SurrealDb) {
    repo_root();
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(std::env::temp_dir().join("navigator-api-project-notations-storage"))
            .await
            .unwrap(),
    );
    seed::seed_canonical(&surreal, &storage).await.unwrap();
    let runtime: Arc<dyn StateMachineRuntime> = Arc::new(InMemoryRuntime::new());
    let state = AppState {
        sessions: SessionStore::new(KEY),
        storage: storage.clone(),
        workflow_runtime: runtime.clone(),
        questionnaire_runtime: runtime,
        ..portal::test_support::app_state(surreal.clone()).await
    };
    (
        server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
    )
}

async fn open_project(surreal: &store::surreal::SurrealDb) -> store::projects::Project {
    store::test_support::seed_project(surreal, "Matter").await
}

/// A `Bearer` header for a session of `role`, optionally scoped to `project`
/// with a firm-side participation row.
async fn bearer(
    surreal: &store::surreal::SurrealDb,
    email: &str,
    role: Role,
    project: Option<uuid::Uuid>,
) -> String {
    let actor = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(email, email, role),
    )
    .await
    .unwrap();
    if let Some(project_id) = project {
        store::projects::add_participation(surreal, project_id, actor.id, "lawyer")
            .await
            .unwrap();
    }
    let mut s = SessionData::fresh("api-notation-sub", role);
    s.person_id = Some(actor.id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

/// POST the JSON body to the notation door, optionally authenticated.
async fn post_notation(
    app: &axum::Router,
    auth: Option<&str>,
    project_id: uuid::Uuid,
    code: &str,
    email: &str,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/app/api/projects/{project_id}/notations"))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    app.clone()
        .oneshot(
            req.body(Body::from(
                serde_json::json!({ "template_code": code, "client_email": email }).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap()
}

async fn notation_count(surreal: &store::surreal::SurrealDb, project_id: uuid::Uuid) -> u64 {
    store::notations::list_by_project(surreal, project_id)
        .await
        .unwrap()
        .len() as u64
}

#[tokio::test]
async fn anonymous_is_401_and_opens_nothing() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());

    let resp = post_notation(&app, None, project.id, "amendment", "libra@example.com").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(notation_count(&surreal, project.id).await, 0);
}

#[tokio::test]
async fn client_is_403_and_opens_nothing() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());
    let client = bearer(&surreal, "client@example.com", Role::Client, None).await;

    let resp = post_notation(
        &app,
        Some(&client),
        project.id,
        "amendment",
        "libra@example.com",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(notation_count(&surreal, project.id).await, 0);
}

#[tokio::test]
async fn lawyer_outside_the_matter_scope_is_404_and_opens_nothing() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());
    // Lawyer tier, but not a participant in this matter: the scope gate collapses
    // to a bare 404 so the door never discloses the out-of-scope matter.
    let outsider = bearer(&surreal, "outsider@example.com", Role::Lawyer, None).await;

    let resp = post_notation(
        &app,
        Some(&outsider),
        project.id,
        "amendment",
        "libra@example.com",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(notation_count(&surreal, project.id).await, 0);
}

#[tokio::test]
async fn participant_lawyer_opens_a_notation_and_returns_201_json() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    let project_id = project.id;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());
    let lawyer = bearer(
        &surreal,
        "acting-lawyer@example.com",
        Role::Lawyer,
        Some(project_id),
    )
    .await;

    let resp = post_notation(
        &app,
        Some(&lawyer),
        project_id,
        "amendment",
        "libra@example.com",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let notation_id = json["notation_id"]
        .as_str()
        .expect("the response carries a notation_id");

    // The advertised id names a real notation on this matter, bound to the
    // client resolved by email (distinct from the acting lawyer).
    let notation =
        store::notations::find_by_id(&surreal, uuid::Uuid::parse_str(notation_id).unwrap())
            .await
            .unwrap()
            .expect("the notation the response named exists");
    assert_eq!(notation.project_id, project_id);
    let client = store::persons::find_by_email_ci(&surreal, "libra@example.com")
        .await
        .unwrap()
        .expect("client person created");
    assert_eq!(notation.person_id, client.id);
}

#[tokio::test]
async fn admin_bypasses_scope_and_opens_a_notation() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    let project_id = project.id;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());
    // Admin does NOT participate in the matter; the scope check bypasses for admin.
    let admin = bearer(&surreal, "admin@example.com", Role::Admin, None).await;

    let resp = post_notation(
        &app,
        Some(&admin),
        project_id,
        "amendment",
        "libra@example.com",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    assert_eq!(notation_count(&surreal, project_id).await, 1);
}

/// The published `CreateNotationRequest` schema and `Json` decoding accept the
/// same objects. Empty and whitespace-only strings are strings, so the schema
/// accepts them and the existing trim check rejects them.
fn schema_accepts(schema: &serde_json::Value, instance: &serde_json::Value) -> bool {
    if let Some(branches) = schema.get("oneOf").and_then(serde_json::Value::as_array) {
        let matches = branches
            .iter()
            .filter(|branch| schema_accepts(branch, instance))
            .count();
        if matches != 1 {
            return false;
        }
    }
    if let Some(ty) = schema.get("type").and_then(serde_json::Value::as_str) {
        let type_ok = match ty {
            "object" => instance.is_object(),
            "string" => instance.is_string(),
            "boolean" => instance.is_boolean(),
            "number" => instance.is_number(),
            "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
            "array" => instance.is_array(),
            "null" => instance.is_null(),
            _ => false,
        };
        if !type_ok {
            return false;
        }
    }
    let constrains_object = schema.get("properties").is_some()
        || schema.get("required").is_some()
        || schema.get("additionalProperties").is_some();
    if constrains_object {
        let Some(object) = instance.as_object() else {
            return false;
        };
        if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
            for key in required {
                let Some(key) = key.as_str() else {
                    return false;
                };
                if !object.contains_key(key) {
                    return false;
                }
            }
        }
        if schema.get("additionalProperties") == Some(&serde_json::Value::Bool(false)) {
            if let Some(properties) = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
            {
                if object.keys().any(|key| !properties.contains_key(key)) {
                    return false;
                }
            }
        }
        if let Some(properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            for (key, property) in properties {
                if let Some(value) = object.get(key) {
                    if !schema_accepts(property, value) {
                        return false;
                    }
                }
            }
        }
    }
    true
}

async fn post_json(
    app: &axum::Router,
    auth: Option<&str>,
    project_id: uuid::Uuid,
    body: &serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/app/api/projects/{project_id}/notations"))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    app.clone()
        .oneshot(
            req.body(Body::from(serde_json::to_vec(body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HttpCodeExpect {
    Create,
    Blank,
    Reject,
}

fn http_code_cases(email: &str) -> Vec<(&'static str, serde_json::Value, HttpCodeExpect)> {
    vec![
        (
            "canonical",
            serde_json::json!({"template_code": "amendment", "client_email": email}),
            HttpCodeExpect::Create,
        ),
        (
            "alias",
            serde_json::json!({"draft_code": "amendment", "client_email": email}),
            HttpCodeExpect::Create,
        ),
        (
            "padded alias",
            serde_json::json!({"draft_code": "  amendment  ", "client_email": email}),
            HttpCodeExpect::Create,
        ),
        (
            "both equal",
            serde_json::json!({
                "template_code": "amendment",
                "draft_code": "amendment",
                "client_email": email
            }),
            HttpCodeExpect::Reject,
        ),
        (
            "both different",
            serde_json::json!({
                "template_code": "amendment",
                "draft_code": "other",
                "client_email": email
            }),
            HttpCodeExpect::Reject,
        ),
        (
            "neither",
            serde_json::json!({"client_email": email}),
            HttpCodeExpect::Reject,
        ),
        (
            "missing client",
            serde_json::json!({"draft_code": "amendment"}),
            HttpCodeExpect::Reject,
        ),
        (
            "canonical null",
            serde_json::json!({"template_code": null, "client_email": email}),
            HttpCodeExpect::Reject,
        ),
        (
            "alias null",
            serde_json::json!({"draft_code": null, "client_email": email}),
            HttpCodeExpect::Reject,
        ),
        (
            "canonical number",
            serde_json::json!({"template_code": 1, "client_email": email}),
            HttpCodeExpect::Reject,
        ),
        (
            "alias bool",
            serde_json::json!({"draft_code": true, "client_email": email}),
            HttpCodeExpect::Reject,
        ),
        (
            "canonical empty",
            serde_json::json!({"template_code": "", "client_email": email}),
            HttpCodeExpect::Blank,
        ),
        (
            "alias empty",
            serde_json::json!({"draft_code": "", "client_email": email}),
            HttpCodeExpect::Blank,
        ),
        (
            "canonical whitespace",
            serde_json::json!({"template_code": "  ", "client_email": email}),
            HttpCodeExpect::Blank,
        ),
        (
            "alias whitespace",
            serde_json::json!({"draft_code": " \n\t ", "client_email": email}),
            HttpCodeExpect::Blank,
        ),
    ]
}

struct HttpCase<'a> {
    app: &'a axum::Router,
    surreal: &'a store::surreal::SurrealDb,
    lawyer: &'a str,
    project_id: uuid::Uuid,
    schema: &'a serde_json::Value,
}

async fn assert_http_code_case(
    case: &HttpCase<'_>,
    label: &str,
    body: &serde_json::Value,
    expect: HttpCodeExpect,
) -> Option<store::notations::Notation> {
    let before = notation_count(case.surreal, case.project_id).await;
    let schema_ok = schema_accepts(case.schema, body);
    let resp = post_json(case.app, Some(case.lawyer), case.project_id, body).await;
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    match expect {
        HttpCodeExpect::Create => {
            assert!(schema_ok, "{label} schema rejected {body}");
            assert_eq!(status, StatusCode::CREATED, "{label}: {json}");
            assert!(json.get("draft_code").is_none(), "{label}");
            assert!(json.get("template_code").is_none(), "{label}");
            let id = uuid::Uuid::parse_str(json["notation_id"].as_str().unwrap()).unwrap();
            let row = store::notations::find_by_id(case.surreal, id)
                .await
                .unwrap()
                .unwrap();
            let stored = serde_json::to_value(&row).unwrap();
            assert!(stored.get("draft_code").is_none(), "{label}");
            assert_eq!(row.project_id, case.project_id);
            Some(row)
        }
        HttpCodeExpect::Blank => {
            assert!(schema_ok, "{label} is a string the schema accepts");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {json}");
            assert_eq!(json["error"], "invalid_request");
            assert_eq!(
                json["message"],
                "template_code and client_email are required."
            );
            assert_eq!(
                notation_count(case.surreal, case.project_id).await,
                before,
                "{label}"
            );
            None
        }
        HttpCodeExpect::Reject => {
            assert!(!schema_ok, "{label} schema accepted {body}");
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label}: {json}");
            assert_eq!(json["error"], "invalid_request");
            if label.starts_with("both") {
                let message = json["message"].as_str().unwrap_or("");
                assert!(
                    message.contains("duplicate"),
                    "{label} must reject both keys: {message}"
                );
            }
            assert_eq!(
                notation_count(case.surreal, case.project_id).await,
                before,
                "{label}"
            );
            None
        }
    }
}

#[tokio::test]
async fn code_spellings_match_the_openapi_schema_and_write_only_for_one_valid_code() {
    let _repo_guard = REPO_ENV_LOCK.lock().await;
    let (app, surreal) = build_app().await;
    let project = open_project(&surreal).await;
    let project_id = project.id;
    commit_template(&project.code, "amendment", amendment_template().as_bytes());
    let lawyer = bearer(
        &surreal,
        "acting-lawyer@example.com",
        Role::Lawyer,
        Some(project_id),
    )
    .await;
    let client = bearer(&surreal, "client@example.com", Role::Client, None).await;
    let outsider = bearer(&surreal, "outsider@example.com", Role::Lawyer, None).await;
    let schema =
        portal::openapi::document()["components"]["schemas"]["CreateNotationRequest"].clone();
    let email = "libra@example.com";
    let draft = serde_json::json!({"draft_code": "amendment", "client_email": email});

    assert_eq!(
        post_json(&app, Some(&client), project_id, &draft)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post_json(&app, Some(&outsider), project_id, &draft)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post_json(&app, None, project_id, &draft).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(notation_count(&surreal, project_id).await, 0);

    let mut created = Vec::new();
    for (label, body, expect) in http_code_cases(email) {
        if let Some(row) = assert_http_code_case(
            &HttpCase {
                app: &app,
                surreal: &surreal,
                lawyer: &lawyer,
                project_id,
                schema: &schema,
            },
            label,
            &body,
            expect,
        )
        .await
        {
            created.push(row);
        }
    }
    assert!(created.len() >= 2);
    assert!(created
        .iter()
        .all(|row| row.template_id == created[0].template_id));
    let ids: std::collections::BTreeSet<_> = created.iter().map(|row| row.id).collect();
    assert_eq!(ids.len(), created.len());
}

/// Commit `templates/<code>.md` to the project's repo and return HEAD.
fn commit_template(project_code: &str, code: &str, body: &[u8]) -> String {
    let repo = repos::RepoStore::from_env().unwrap();
    repo.ensure_code(project_code).unwrap();
    repo.commit_as_code(
        project_code,
        repos::Author {
            name: "Lawyer",
            email: "lawyer@example.com",
        },
        "add template",
        &[(&format!("templates/{code}.md"), body)],
    )
    .unwrap();
    repo.head_oid_code(project_code).unwrap().unwrap()
}
