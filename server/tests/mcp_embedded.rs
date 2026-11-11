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
        ..portal::test_support::app_state(portal::test_support::embedded_surreal().await).await
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
}

#[tokio::test]
async fn public_mcp_exposes_only_template_discovery_without_bearer() {
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
                assert_eq!(tools.len(), 1);
                assert_eq!(tools[0]["name"], "list_templates");
            }
            _ if params["name"] == "list_templates" => assert_eq!(
                body["result"]["structuredContent"]["clarification"],
                "Do you want a Work In Progress summary for a matter, or do you want to record time or a cost?"
            ),
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

/// A valid bearer authenticates `/app/mcp`.
#[tokio::test]
async fn app_mcp_accepts_valid_bearer_token_when_auth_enforced() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let claims = AuthClaims {
        sub: "lawyer@neonlaw.com".into(),
        exp: i64::try_from(jsonwebtoken::get_current_timestamp() + 3600).unwrap(),
        role: store::persons::Role::Lawyer,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap();

    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let resp = app
        .oneshot(mcp_request_at(
            "/app/mcp",
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list"
            }),
            Some(&token),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"create_person"));
}

#[tokio::test]
async fn mcp_accepts_valid_bearer_token_when_auth_enforced() {
    let state = state_with(AuthConfig::new(false, Some("test-secret"))).await;
    let claims = AuthClaims {
        sub: "lawyer@neonlaw.com".into(),
        exp: i64::try_from(jsonwebtoken::get_current_timestamp() + 3600).unwrap(),
        role: store::persons::Role::Lawyer,
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap();

    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let resp = app
        .oneshot(mcp_request(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list"
            }),
            Some(&token),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let tools = body["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"create_person"));
    assert!(names.contains(&"show_person"));
    assert!(names.contains(&"list_jurisdictions"));
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
