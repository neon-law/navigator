//! Acceptance tests for the sandbox search tools (`search_case_law`,
//! `search_legal_aid`), dispatched through the real [`mcp::tools::call_tool`]
//! path — the same catalog, authorization, and audit seam every other tool
//! rides.
//!
//! The checks the Legal Sandbox spec asks for:
//!
//! * the catalog listing includes the new tools, and they are classified
//!   read-only (no confirmation pause, no tier gate);
//! * an unknown tool is refused by the dispatcher;
//! * a dispatched search proxies the sanitized terms only — the captured
//!   outbound request carries `q`, `type`, `page_size` and nothing else;
//! * dispatch through a state without backends degrades to a "search
//!   unavailable" result rather than failing the call.
//!
//! Audit behavior is the catalog's existing seam: every `call_tool`
//! dispatch runs under the shared `mcp.tool.call` tracing span with a
//! `tool` field and a failure warn (`mcp/src/server.rs`), so the search
//! tools are audit-logged by construction rather than by a second
//! mechanism.

use mcp::search::{CourtListener, SearchBackends};
use mcp::tools::{
    call_tool, is_advertised, is_known_tool, is_side_effecting, list_tools, requires_confirmation,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn the_new_tools() -> Vec<&'static str> {
    vec!["search_case_law", "search_legal_aid"]
}

#[test]
fn the_catalog_listing_includes_the_search_tools() {
    let names: Vec<String> = list_tools()
        .iter()
        .filter_map(|d| d["name"].as_str().map(str::to_string))
        .collect();
    for tool in the_new_tools() {
        assert!(
            names.iter().any(|n| n == tool),
            "{tool} missing from the catalog"
        );
        assert!(is_known_tool(tool), "{tool} is not a known tool");
        assert!(
            is_advertised(tool),
            "{tool} is not advertised to model clients"
        );
    }
}

#[test]
fn the_search_tools_are_read_only_and_run_without_confirmation() {
    for tool in the_new_tools() {
        assert!(
            !is_side_effecting(tool),
            "{tool} must be classified read-only"
        );
        assert!(
            !requires_confirmation(tool),
            "{tool} must not require a lawyer's confirmation pause"
        );
    }
}

#[tokio::test]
async fn the_dispatcher_refuses_a_tool_outside_the_catalog() {
    let surreal = store::test_support::mem_surreal().await;
    let state = mcp::McpState::new(
        surreal,
        std::sync::Arc::new(workflows::InMemoryRuntime::new()),
    );
    let err = call_tool(&state, None, "search_case_lawz", &json!({}))
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("unknown tool"));
}

#[tokio::test]
async fn dispatch_without_backends_degrades_to_search_unavailable() {
    let surreal = store::test_support::mem_surreal().await;
    let state = mcp::McpState::new(
        surreal,
        std::sync::Arc::new(workflows::InMemoryRuntime::new()),
    );
    for tool in the_new_tools() {
        let result = call_tool(&state, None, tool, &json!({ "query": "water rights" }))
            .await
            .unwrap_or_else(|e| panic!("{tool} must degrade, not fail: {e}"));
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.starts_with("search unavailable: "),
            "{tool} degraded text was: {text}"
        );
    }
}

#[tokio::test]
async fn a_dispatched_search_proxies_the_terms_and_nothing_else() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/rest/v4/search/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
        .mount(&server)
        .await;
    let surreal = store::test_support::mem_surreal().await;
    let mut state = mcp::McpState::new(
        surreal,
        std::sync::Arc::new(workflows::InMemoryRuntime::new()),
    );
    state.search = Some(SearchBackends {
        case_law: Some(std::sync::Arc::new(CourtListener::new(server.uri(), None))),
        legal_aid: None,
    });

    let result = call_tool(
        &state,
        None,
        "search_case_law",
        &json!({ "query": "water rights", "limit": 2 }),
    )
    .await
    .unwrap();
    assert_eq!(result["structuredContent"]["count"], 0);

    // The whole sanitization contract, asserted on the wire: exactly the
    // search terms and the page size — no person id, session data, or
    // case facts ride along.
    let req = &server.received_requests().await.unwrap()[0];
    let query = req.url.query().unwrap();
    let pairs: Vec<(String, String)> = query
        .split('&')
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.to_string(), v.to_string())
        })
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("q".into(), "water+rights".into()), // form-encoded space
            ("type".into(), "o".into()),
            ("page_size".into(), "2".into()),
        ]
    );
}
