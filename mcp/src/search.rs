//! The Legal Sandbox's private legal search: the vendor seam behind the
//! `search_case_law` and `search_legal_aid` tools.
//!
//! Private because the tool runs here, server-side, and forwards
//! **search terms only** — no subscriber identity, no case facts beyond
//! the terms, no conversation context. What a provider receives is a
//! public search request that could have come from anyone.
//!
//! The degradation contract the spec pins: a failed or unconfigured
//! provider never fails the calling session. [`SearchError`] exists so
//! the tool layer can answer "search unavailable" and let the rest of
//! the completion proceed.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// One search result, normalized across vendors. A subscriber sees
/// titles, links, and excerpts — never a vendor's raw row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// The only thing a search backend learns about a caller: what to look
/// for and how much to return. The type is the sanitization boundary —
/// a provider client cannot ask for more, because the field does not
/// exist here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchTerms {
    pub query: String,
    pub limit: u32,
}

impl SearchTerms {
    /// Normalize free-text input before it leaves: trimmed, whitespace
    /// collapsed, capped, and never empty. An empty query is refused —
    /// a blank form is a caller error, not a search.
    pub fn new(query: &str, limit: u32) -> Result<Self, SearchError> {
        let collapsed = query.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            return Err(SearchError("query must not be empty".into()));
        }
        Ok(Self {
            query: collapsed.chars().take(MAX_QUERY_CHARS).collect(),
            limit,
        })
    }
}

/// Cap the query a session can send: a model pasting a whole document
/// into `query` must not become a provider request the size of a
/// matter file.
const MAX_QUERY_CHARS: usize = 500;

/// The one method a search backend answers. Implemented for
/// [`CourtListener`] and [`Descrybe`]; the tool layer holds
/// `Arc<dyn SearchProvider>` and cannot see which vendor is behind it.
#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(&self, terms: &SearchTerms) -> Result<Vec<SearchHit>, SearchError>;
}

/// The failure mode every provider shares: this search is unavailable.
/// The message says why (timeout, HTTP status, empty response) so the
/// session can say something honest; it is never a caller error.
#[derive(Debug, thiserror::Error)]
#[error("search unavailable: {0}")]
pub struct SearchError(pub String);

/// The vendor clients the sandbox tools dispatch through. Built once at
/// bootstrap from environment configuration; an unconfigured provider
/// degrades its tool's searches to "search unavailable" rather than
/// failing boot or the calling session.
#[derive(Clone)]
pub struct SearchBackends {
    pub case_law: Option<Arc<dyn SearchProvider>>,
    pub legal_aid: Option<Arc<dyn SearchProvider>>,
}

impl SearchBackends {
    /// Build every backend the environment configures. Both env vars
    /// are optional in tests and KIND; a backend without its config is
    /// simply absent.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            case_law: Some(Arc::new(CourtListener::from_env())),
            legal_aid: Descrybe::from_env()
                .map(|d| Arc::new(d) as Arc<dyn SearchProvider>),
        }
    }
}

// ---------------------------------------------------------------------------
// CourtListener
// ---------------------------------------------------------------------------

/// `CourtListener`'s free case-law search (`/api/rest/v4/search/`,
/// `type=o`). A deployment token (env `COURTLISTENER_TOKEN`) raises the
/// rate limit; the search works without one, so the token is optional.
pub struct CourtListener {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl CourtListener {
    /// Production client: the public API endpoint, token from
    /// `COURTLISTENER_TOKEN` when set.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(
            "https://www.courtlistener.com".into(),
            std::env::var("COURTLISTENER_TOKEN").ok().filter(|t| !t.is_empty()),
        )
    }

    /// Test client: any base URL, any token.
    #[must_use]
    pub fn new(base_url: String, token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            token,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for CourtListener {
    async fn search(&self, terms: &SearchTerms) -> Result<Vec<SearchHit>, SearchError> {
        // The outbound request is the sanitization promise kept: exactly
        // the search terms and the page size. No header, param, or body
        // carries anything about the caller.
        let mut req = self
            .http
            .get(format!("{}/api/rest/v4/search/", self.base_url))
            .query(&[("q", terms.query.as_str()), ("type", "o")])
            .query(&[("page_size", terms.limit.to_string())]);
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let body = req
            .send()
            .await
            .map_err(|e| SearchError(format!("CourtListener request failed: {e}")))?
            .error_for_status()
            .map_err(|e| SearchError(format!("CourtListener returned an error: {e}")))?
            .text()
            .await
            .map_err(|e| SearchError(format!("CourtListener body unreadable: {e}")))?;
        parse_courtlistener(&body, terms.limit)
    }
}

/// Map a `CourtListener` search response onto [`SearchHit`]s. Tolerant by
/// field: a hit missing `caseName` or `absolute_url` still surfaces with
/// empty strings, because a partial row beats a failed search.
fn parse_courtlistener(body: &str, limit: u32) -> Result<Vec<SearchHit>, SearchError> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        results: Vec<RawHit>,
    }
    #[derive(Deserialize)]
    struct RawHit {
        #[serde(default)]
        case_name: Option<String>,
        #[serde(default)]
        absolute_url: Option<String>,
        #[serde(default)]
        snippet: Option<String>,
    }
    let raw: Raw = serde_json::from_str(body)
        .map_err(|e| SearchError(format!("CourtListener response unparseable: {e}")))?;
    Ok(raw
        .results
        .into_iter()
        .take(limit as usize)
        .map(|h| SearchHit {
            title: h.case_name.unwrap_or_default(),
            url: h
                .absolute_url
                .map(|u| format!("https://www.courtlistener.com{u}"))
                .unwrap_or_default(),
            snippet: h.snippet.unwrap_or_default(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Descrybe
// ---------------------------------------------------------------------------

/// Descrybe legal-aid research search. Wire format **provisional**:
/// Descrybe's public documentation does not expose a stable contract, so
/// this client posts `{ query, limit }` and reads `{ results: [{title,
/// url, snippet}] }` — the shape to confirm against a live integration
/// before production traffic relies on it. Configuration (env
/// `DESCRYBE_BASE_URL`, `DESCRYBE_API_KEY`) is required; without it the
/// backend is absent and `search_legal_aid` reports search unavailable.
pub struct Descrybe {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Descrybe {
    /// Production client, or `None` when the deployment has not
    /// configured the provider.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("DESCRYBE_BASE_URL").ok().filter(|u| !u.is_empty())?;
        let api_key = std::env::var("DESCRYBE_API_KEY").ok().filter(|k| !k.is_empty())?;
        Some(Self::new(base_url, api_key))
    }

    /// Test client: any base URL, any key.
    #[must_use]
    pub fn new(base_url: String, api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            api_key,
        }
    }
}

#[async_trait::async_trait]
impl SearchProvider for Descrybe {
    async fn search(&self, terms: &SearchTerms) -> Result<Vec<SearchHit>, SearchError> {
        // Same promise as the case-law client: the terms and the page
        // size, nothing else. The key authenticates the deployment, not
        // the subscriber.
        let body = self
            .http
            .post(format!("{}/search", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "query": terms.query, "limit": terms.limit }))
            .send()
            .await
            .map_err(|e| SearchError(format!("Descrybe request failed: {e}")))?
            .error_for_status()
            .map_err(|e| SearchError(format!("Descrybe returned an error: {e}")))?
            .text()
            .await
            .map_err(|e| SearchError(format!("Descrybe body unreadable: {e}")))?;
        parse_descrybe(&body, terms.limit)
    }
}

/// Map a Descrybe search response onto [`SearchHit`]s. Tolerant by
/// field, like the CourtListener parser.
fn parse_descrybe(body: &str, limit: u32) -> Result<Vec<SearchHit>, SearchError> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        results: Vec<RawHit>,
    }
    #[derive(Deserialize)]
    struct RawHit {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        snippet: Option<String>,
    }
    let raw: Raw = serde_json::from_str(body)
        .map_err(|e| SearchError(format!("Descrybe response unparseable: {e}")))?;
    Ok(raw
        .results
        .into_iter()
        .take(limit as usize)
        .map(|h| SearchHit {
            title: h.title.unwrap_or_default(),
            url: h.url.unwrap_or_default(),
            snippet: h.snippet.unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_courtlistener, parse_descrybe, CourtListener, Descrybe, SearchBackends,
        SearchError, SearchProvider, SearchTerms,
    };
    use serde_json::{json, Value};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn terms(query: &str, limit: u32) -> SearchTerms {
        SearchTerms::new(query, limit).unwrap()
    }

    // -- SearchTerms normalization: the sanitization boundary itself --

    #[test]
    fn terms_are_trimmed_and_whitespace_collapsed() {
        let t = terms("  eviction   answer\tdeadline  ", 5);
        assert_eq!(t.query, "eviction answer deadline");
        assert_eq!(t.limit, 5);
    }

    #[test]
    fn terms_refuse_a_blank_query() {
        assert!(SearchTerms::new("   ", 5).is_err());
        assert!(SearchTerms::new("", 5).is_err());
    }

    #[test]
    fn terms_are_capped_so_a_pasted_document_stays_a_query() {
        let pasted = "term ".repeat(400);
        let t = SearchTerms::new(&pasted, 5).unwrap();
        assert_eq!(t.query.chars().count(), 500);
    }

    // -- CourtListener over a mocked HTTP layer --

    #[tokio::test]
    async fn courtlistener_searches_and_parses_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rest/v4/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    {
                        "caseName": "Doe v. Sample Corp.",
                        "absolute_url": "/opinion/123/doe-v-sample/",
                        "snippet": "eviction answer deadline"
                    },
                    { "caseName": "In re Unnamed", "absolute_url": "/opinion/9/" }
                ]
            })))
            .mount(&server)
            .await;

        let hits = CourtListener::new(server.uri(), None)
            .search(&terms("eviction deadline", 5))
            .await
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Doe v. Sample Corp.");
        assert_eq!(
            hits[0].url,
            "https://www.courtlistener.com/opinion/123/doe-v-sample/"
        );
        assert_eq!(hits[1].snippet, ""); // tolerant: missing snippet
    }

    #[tokio::test]
    async fn courtlistener_outbound_request_carries_terms_and_nothing_else() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/rest/v4/search/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
            .mount(&server)
            .await;

        CourtListener::new(server.uri(), None)
            .search(&terms("water rights", 5))
            .await
            .unwrap();

        let req = &server.received_requests().await.unwrap()[0];
        let query = req.url.query().unwrap();
        let pairs: Vec<(String, String)> = query
            .split('&')
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (k.to_string(), v.to_string())
            })
            .collect();
        // Exactly the search terms and the page size — the whole
        // sanitization contract, asserted on the wire.
        assert_eq!(
            pairs,
            vec![
                ("q".into(), "water%20rights".into()),
                ("type".into(), "o".into()),
                ("page_size".into(), "5".into())
            ]
        );
    }

    #[tokio::test]
    async fn courtlistener_sends_the_token_as_a_bearer_header_when_configured() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "results": [] })))
            .mount(&server)
            .await;

        CourtListener::new(server.uri(), Some("sekrit-token".into()))
            .search(&terms("q", 5))
            .await
            .unwrap();

        let req = &server.received_requests().await.unwrap()[0];
        assert_eq!(
            req.headers().get("authorization").unwrap().to_str().unwrap(),
            "Bearer sekrit-token"
        );
    }

    #[tokio::test]
    async fn courtlistener_maps_an_http_error_to_search_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = CourtListener::new(server.uri(), None)
            .search(&terms("q", 5))
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("search unavailable"), "{err}");
    }

    // -- Descrybe over a mocked HTTP layer --

    #[tokio::test]
    async fn descrybe_posts_the_terms_and_parses_hits() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "results": [
                    { "title": "Eviction defense guide", "url": "https://example.org/guide",
                      "snippet": "deadline to answer" }
                ]
            })))
            .mount(&server)
            .await;

        let hits = Descrybe::new(server.uri(), "k".into())
            .search(&terms("eviction defense", 5))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Eviction defense guide");

        let req = &server.received_requests().await.unwrap()[0];
        assert_eq!(
            req.headers().get("authorization").unwrap().to_str().unwrap(),
            "Bearer k"
        );
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(body["query"], "eviction defense");
        assert_eq!(body["limit"], 5);
        // The body carries the terms and nothing else.
        assert_eq!(body.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn descrybe_maps_an_http_error_to_search_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let err = Descrybe::new(server.uri(), "k".into())
            .search(&terms("q", 5))
            .await
            .unwrap_err();
        assert!(err.to_string().starts_with("search unavailable"), "{err}");
    }

    #[test]
    fn descrybe_is_absent_without_configuration() {
        // Temp-scoped assertion: if a deployment sets these, the test
        // environment would too — so assert the *absence contract*
        // against deliberately-unset variables instead.
        std::env::remove_var("DESCRYBE_BASE_URL");
        std::env::remove_var("DESCRYBE_API_KEY");
        assert!(Descrybe::from_env().is_none());
    }

    // -- Parsers directly: tolerance without a wire round-trip --

    #[test]
    fn parsers_tolerate_missing_fields_and_extra_rows() {
        let hits = parse_courtlistener(r#"{"results": [{"caseName": "X"}]}"#, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "X");
        assert_eq!(hits[0].url, "");

        let hits = parse_descrybe(r#"{"results": [{"url": "https://x"}]}"#, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "");

        // A malformed body is a SearchError, not a panic.
        assert!(parse_courtlistener("not json", 10).is_err());
        assert!(parse_descrybe("not json", 10).is_err());
    }

    #[test]
    fn parsers_respect_the_page_limit() {
        let body = r#"{"results": [{"caseName": "a"}, {"caseName": "b"}, {"caseName": "c"}]}"#;
        assert_eq!(parse_courtlistener(body, 2).unwrap().len(), 2);
        assert_eq!(parse_courtlistener(r#"{}"#, 2).unwrap(), vec![]);
    }

    #[test]
    fn search_error_display_is_the_degradation_contract() {
        let e = SearchError("timeout".into());
        assert_eq!(e.to_string(), "search unavailable: timeout");
    }

    #[test]
    fn backends_hold_the_two_vendor_slots() {
        let b = SearchBackends::from_env();
        // case_law needs no configuration, so from_env always has it.
        assert!(b.case_law.is_some());
        // legal_aid follows the environment (absent without config).
        std::env::remove_var("DESCRYBE_BASE_URL");
        std::env::remove_var("DESCRYBE_API_KEY");
        assert!(SearchBackends::from_env().legal_aid.is_none());
    }
}
