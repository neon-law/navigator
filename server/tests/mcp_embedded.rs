#![allow(clippy::doc_markdown)]
//! Tests the public and authenticated MCP routes on the composed server.
//! Drives the router via `tower::ServiceExt::oneshot` without binding a socket.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use portal::{AppState, AuthClaims, AuthConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn state_with(auth: AuthConfig) -> AppState {
    AppState {
        auth,
        storage: std::sync::Arc::new(
            cloud::FsStorage::new(std::env::temp_dir().join("navigator-mcp-test-storage"))
                .await
                .unwrap(),
        ),
        ..portal::test_support::app_state(
            store::application_store::ApplicationStore::uninitialized(),
        )
        .await
    }
}

fn mcp_request(body: &Value, bearer: Option<&str>) -> Request<Body> {
    mcp_request_at("/app/mcp", body, bearer)
}

fn mcp_request_at(uri: &str, body: &Value, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn mcp_initialize_round_trips_when_auth_disabled() {
    // disabled=true, so the require_auth layer passes through.
    let state = state_with(AuthConfig::new(true, None)).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));

    let resp = app
        .oneshot(mcp_request(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {}
            }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["result"]["serverInfo"]["name"], "navigator-mcp");
    let instructions = body["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("/lawyers"));
    assert!(instructions.contains("representing themselves"));
}

#[tokio::test]
async fn public_mcp_exposes_page_and_template_discovery_without_bearer() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let mut browser_request = mcp_request_at(
        "/app/mcp",
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        None,
    );
    browser_request.headers_mut().insert(
        axum::http::header::ORIGIN,
        axum::http::HeaderValue::from_static("https://example.com"),
    );
    let response = app.clone().oneshot(browser_request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    for (method, params) in [
        ("initialize", json!({ "protocolVersion": "2025-06-18" })),
        ("notifications/initialized", json!({})),
        ("tools/list", json!({})),
        (
            "tools/call",
            json!({
                "name": "list_templates",
                "arguments": { "intent": "create a work in progress" }
            }),
        ),
        (
            "tools/call",
            json!({ "name": "list_public_pages", "arguments": {} }),
        ),
        ("tools/call", json!({ "name": "list_projects" })),
        ("tools/call", json!({ "name": "create_notation" })),
    ] {
        let mut request = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if method != "notifications/initialized" {
            request["id"] = json!(1);
        }
        let response = app
            .clone()
            .oneshot(mcp_request_at("/app/mcp", &request, None))
            .await
            .unwrap();
        if method == "notifications/initialized" {
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            assert!(response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty());
            continue;
        }
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        match method {
            "initialize" => assert_eq!(body["result"]["protocolVersion"], "2025-06-18"),
            "tools/list" => {
                let tools = body["result"]["tools"].as_array().unwrap();
                let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
                assert_eq!(names, ["list_public_pages", "list_templates"]);
            }
            _ if params["name"] == "list_templates" => assert_eq!(
                body["result"]["structuredContent"]["clarification"],
                "Do you want a Work In Progress summary for a matter, or do you want to record time or a cost?"
            ),
            _ if params["name"] == "list_public_pages" => {
                let pages = body["result"]["structuredContent"]["pages"]
                    .as_array()
                    .expect("public pages");
                assert!(pages.iter().any(|page| page["path"] == "/"));
                assert!(pages.iter().any(|page| page["path"] == "/lawyers"));
                assert!(pages.iter().all(|page| page["path"] != "/navigator"));
            }
            _ => assert_eq!(body["result"]["isError"], true),
        }
    }
}

#[tokio::test]
async fn app_mcp_rejects_invalid_bearer_instead_of_falling_back_to_discovery() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));

    let resp = app
        .oneshot(mcp_request_at(
            "/app/mcp",
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            Some("invalid"),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signed_session_discovers_tools_and_initializes_over_http() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let mut session =
        portal::SessionData::fresh("lawyer@example.test", store::persons::Role::Lawyer);
    session.email = Some("lawyer@example.test".into());
    session.source = portal::SessionSource::Cli;
    let token = state.sessions.encode(&session);
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .clone()
        .oneshot(mcp_request(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            Some(&token),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());
    let response = app
        .oneshot(mcp_request(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
            Some(&token),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let names: Vec<_> = body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(names.contains(&"create_person"));
    assert!(!names.contains(&"create_notation"));
}

#[tokio::test]
async fn a_bearer_without_a_verified_personal_identity_cannot_call_private_tools() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let token = encode(
        &Header::default(),
        &AuthClaims {
            sub: "opaque-subject".into(),
            exp: i64::try_from(jsonwebtoken::get_current_timestamp() + 3600).unwrap(),
            role: store::persons::Role::Lawyer,
        },
        &EncodingKey::from_secret(b"test-secret").unwrap(),
    )
    .unwrap();
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app.oneshot(mcp_request(
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "list_projects" } }),
        Some(&token)
    )).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// An HTTP client can supply a deployment bearer without surrendering the
/// actor/decision record for writes. Keep argument values outside the audit log.
#[tokio::test]
async fn signed_http_writes_record_the_person_and_decision() {
    use std::sync::Mutex;
    use store::persons::{NewPerson, Role};
    store::test_support::with_postgres(async {
        let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
        let app = server::neon_router(state.clone(), std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
        let log = tempfile::NamedTempFile::new().unwrap();
        let subscriber = tracing_subscriber::fmt().json().with_target(true)
            .with_writer(Mutex::new(log.reopen().unwrap())).finish();
        let _subscriber = tracing::subscriber::set_default(subscriber);
        for (role, tool, authorized) in [
            (Role::Lawyer, "create_person", true),
            (Role::Client, "create_person", false),
            (Role::Lawyer, "create_notation", false),
        ] {
            let email = format!("{}-{tool}@example.test", role.as_str());
            let actor = store::persons::create(&state.application_store, &NewPerson::with_role(&email, &email, role))
                .await.unwrap();
            let mut session = portal::SessionData::fresh(&email, role);
            session.email = Some(email.clone());
            session.source = portal::SessionSource::Cli;
            let token = state.sessions.encode(&session);
            let response = app.clone().oneshot(mcp_request(&json!({
                "jsonrpc":"2.0", "id":1, "method":"tools/call",
                "params":{"name":tool,"arguments":{"name":"Sample Contact","email":"fixture-contact@example.test"}}
            }), Some(&token))).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let result = body_json(response).await;
            assert_eq!(result["result"]["isError"].as_bool().unwrap_or(false), !authorized, "{result}");
            let events = std::fs::read_to_string(log.path()).unwrap();
            let entry = events.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|entry| entry["target"] == "audit" && entry["fields"]["event"] == "mcp.tool.authorization"
                    && entry["fields"]["person_id"] == actor.id.to_string()).expect("actor's write decision");
            let fields = &entry["fields"];
            assert_eq!(fields["authorized"], authorized);
            assert_eq!(fields["role"], role.as_str());
            assert_eq!(fields["skill"], tool);
            assert_eq!(fields["confirmation_required"], tool == "create_notation");
            assert!(!entry.to_string().contains("@example.test"));
            assert!(!entry.to_string().contains("Sample Contact"));
        }
    }).await;
}

#[tokio::test]
async fn app_mcp_is_the_only_mcp_transport_route() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let response = app
        .clone()
        .oneshot(mcp_request_at(
            "/mcp",
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let request = Request::builder()
        .uri("/app/mcp")
        .header("accept", "text/event-stream")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}
