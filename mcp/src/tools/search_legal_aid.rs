//! `search_legal_aid` MCP tool.
//!
//! Legal-aid research search through Descrybe, the sandbox's first
//! `search_legal_aid` provider. Same shape as
//! [`crate::tools::search_case_law`]: the caller's free text crosses
//! [`SearchTerms`] — the sanitization boundary — and an unconfigured or
//! failing provider degrades to a "search unavailable" result rather
//! than failing the session.
//!
//! The Descrybe wire contract is provisional — its public documentation
//! does not expose a stable one — so the client's request format must be
//! confirmed against a live endpoint before this tool is activated for
//! subscribers. See the `Descrybe` client comment in `crate::search`.

use serde_json::Value;

use super::ToolError;
use crate::search::{hits_payload, unavailable_payload, SearchBackends, SearchTerms};

#[must_use]
pub fn descriptor() -> Value {
    serde_json::json!({
        "name": "search_legal_aid",
        "description": "Search legal-aid research and plain-language explainers \
                        through Descrybe. Takes `query` (the search terms — a tenant's, \
                        debtor's, or claimant's question in ordinary words) and an \
                        optional `limit` (results to return, capped at 10). Returns \
                        titles, links, and excerpts.",
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

    let Some(provider) = backends.and_then(|b| b.legal_aid.as_ref()) else {
        return Ok(unavailable_payload("legal-aid search is not configured"));
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
    use crate::search::{Descrybe, SearchBackends};
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn backends(base_url: String) -> SearchBackends {
        SearchBackends {
            case_law: None,
            legal_aid: Some(std::sync::Arc::new(Descrybe::new(base_url, "k".into()))),
        }
    }

    #[test]
    fn descriptor_names_the_tool_and_requires_a_query() {
        let d = descriptor();
        assert_eq!(d["name"], "search_legal_aid");
        assert_eq!(d["inputSchema"]["required"], json!(["query"]));
        assert_eq!(d["inputSchema"]["additionalProperties"], false);
    }

    #[tokio::test]
    async fn absent_backend_degrades_to_unavailable() {
        let r = call(None, &json!({ "query": "eviction defense" }))
            .await
            .unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            text,
            "search unavailable: legal-aid search is not configured"
        );
        assert_eq!(r["structuredContent"]["available"], false);
    }

    #[tokio::test]
    async fn a_provider_outage_degrades_instead_of_failing_the_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let r = call(
            Some(&backends(server.uri())),
            &json!({ "query": "eviction defense" }),
        )
        .await
        .unwrap();
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("search unavailable: "));
    }

    #[tokio::test]
    async fn a_live_backend_renders_its_hits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "title": "Eviction defense guide", "url": "https://example.org/g",
                      "snippet": "answer deadlines" }
                ]
            })))
            .mount(&server)
            .await;
        let r = call(
            Some(&backends(server.uri())),
            &json!({ "query": "eviction defense" }),
        )
        .await
        .unwrap();
        assert_eq!(r["structuredContent"]["count"], 1);
        assert_eq!(
            r["structuredContent"]["hits"][0]["title"],
            "Eviction defense guide"
        );
    }
}
