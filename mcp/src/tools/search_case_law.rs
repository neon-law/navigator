//! `search_case_law` MCP tool.
//!
//! Public case-law search through `CourtListener`, the sandbox's first
//! `search_case_law` provider. The tool reads the caller's free text,
//! hands it to [`SearchTerms`] — the sanitization boundary, so the
//! outbound request carries search terms and nothing else — and renders
//! the normalized hits. A provider outage or an unconfigured backend
//! degrades to a "search unavailable" result; the calling session never
//! hard-fails because `CourtListener` timed out.

use serde_json::Value;

use super::ToolError;
use crate::search::{hits_payload, unavailable_payload, SearchBackends, SearchTerms};

#[must_use]
pub fn descriptor() -> Value {
    serde_json::json!({
        "name": "search_case_law",
        "description": "Search public U.S. case law on CourtListener. Takes `query` \
                        (the search terms — what the law says about, a party or claim \
                        name, a doctrine) and an optional `limit` (results to return, \
                        capped at 10). Returns case names, links, and excerpts.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "The search terms." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10 }
            },
            "required": ["query"],
            "additionalProperties": false
        }
    })
}

pub async fn call(
    backends: Option<&SearchBackends>,
    arguments: &Value,
) -> Result<Value, ToolError> {
    let query = arguments
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::InvalidArguments("query (string) is required".into()))?;
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(5)
        .min(10) as u32;
    let terms = SearchTerms::new(query, limit).map_err(|e| ToolError::InvalidArguments(e.0))?;

    let Some(provider) = backends.and_then(|b| b.case_law.as_ref()) else {
        return Ok(unavailable_payload("case-law search is not configured"));
    };
    match provider.search(&terms).await {
        Ok(hits) => Ok(hits_payload(&hits)),
        // Degrade, don't fail: a provider outage is the session's "search
        // is unavailable" line, never a tool error.
        Err(e) => Ok(unavailable_payload(&e.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::{call, descriptor};
    use crate::search::{CourtListener, SearchBackends};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn backends(base_url: String) -> SearchBackends {
        SearchBackends {
            case_law: Some(std::sync::Arc::new(CourtListener::new(base_url, None))),
            legal_aid: None,
        }
    }

    #[test]
    fn descriptor_names_the_tool_and_requires_a_query() {
        let d = descriptor();
        assert_eq!(d["name"], "search_case_law");
        assert_eq!(d["inputSchema"]["required"], json!(["query"]));
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn absent_backend_degrades_to_unavailable() {
        let r = call(None, &json!({ "query": "water rights" }))
            .await
            .unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            text,
            "search unavailable: case-law search is not configured"
        );
        assert_eq!(r["structuredContent"]["available"], false);
    }

    #[tokio::test]
    async fn blank_query_is_a_caller_error_not_a_search() {
        let err = call(None, &json!({ "query": "   " })).await.unwrap_err();
        assert!(err.to_string().starts_with("invalid arguments"));
    }

    #[tokio::test]
    async fn a_provider_outage_degrades_instead_of_failing_the_session() {
        // A router that accepts nothing: every request 500s.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rest/v4/search/"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let r = call(
            Some(&backends(server.uri())),
            &json!({ "query": "water rights" }),
        )
        .await
        .unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("search unavailable: "));
    }

    #[tokio::test]
    async fn a_live_backend_renders_its_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rest/v4/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "caseName": "Doe v. Sample Corp.", "absolute_url": "/opinion/1/",
                      "snippet": "eviction answer deadline" }
                ]
            })))
            .mount(&server)
            .await;
        let r = call(
            Some(&backends(server.uri())),
            &json!({ "query": "water rights", "limit": 3 }),
        )
        .await
        .unwrap();
        assert_eq!(r["structuredContent"]["count"], 1);
        assert_eq!(
            r["structuredContent"]["hits"][0]["title"],
            "Doe v. Sample Corp."
        );
        assert_eq!(
            r["structuredContent"]["hits"][0]["url"],
            "https://www.courtlistener.com/opinion/1/"
        );
    }
}
