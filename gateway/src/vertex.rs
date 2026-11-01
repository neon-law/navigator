//! Claude Sonnet on GCP Vertex AI — the production [`ModelProvider`].
//!
//! Vertex exposes Anthropic's Messages API on the Agent Platform
//! `:rawPredict` endpoint: the model id rides the URL (never the body),
//! the body is the Anthropic Messages payload carrying
//! `"anthropic_version": "vertex-2023-10-16"`, and auth is a GCP bearer
//! token over Application Default Credentials — Workload Identity in
//! production. Skills render into the `system` block; Navigator MCP tool
//! specs render into the `tools` array. The API shape is Anthropic's
//! "Claude on Vertex AI" documentation and Sonnet pricing is Anthropic's
//! published list (retrieved 2026-10-30: Sonnet 5.5, $2 input / $10
//! output per `MTok` on the global endpoint).
//!
//! The provider executes no tools: when the model answers with a
//! `tool_use` block the calls are surfaced on the completion's
//! `tool_calls` for the sandbox session loop to execute — dropping one
//! would lie about the exchange, and executing MCP tools inside the
//! completion loop is the session's job, not the provider's. There is no
//! retry policy here either: a failed completion returns `Err` with no
//! usage, and the metered session decides what a retry costs.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

use crate::{tokens_to_cents, CompletionRequest, CompletionResult, ModelProvider, Role};

/// Supplies a bearer token for the Vertex AI REST API. Production mints
/// it from Application Default Credentials; tests inject a static one.
#[async_trait]
pub trait TokenProvider: Send + Sync {
    async fn token(&self) -> Result<String>;
}

/// Always returns the same string. Test-only.
pub struct StaticToken(pub String);

#[async_trait]
impl TokenProvider for StaticToken {
    async fn token(&self) -> Result<String> {
        Ok(self.0.clone())
    }
}

/// Application Default Credentials → Workload Identity in production.
/// Honors `GATEWAY_FAKE_TOKEN` for local runs without ADC, mirroring
/// `billing`'s `BILLING_FAKE_TOKEN`.
///
/// # Errors
/// Errors when ADC cannot be acquired (no workload identity, no
/// `GOOGLE_APPLICATION_CREDENTIALS`, no gcloud user credentials).
pub async fn adc_token_provider() -> Result<Arc<dyn TokenProvider>> {
    if std::env::var_os("GATEWAY_FAKE_TOKEN").is_some() {
        return Ok(Arc::new(StaticToken("unused".into())));
    }
    Ok(Arc::new(AdcToken::new().await?))
}

struct AdcToken {
    source: Arc<dyn google_cloud_token::TokenSource>,
}

impl AdcToken {
    async fn new() -> Result<Self> {
        let scopes: [&str; 1] = ["https://www.googleapis.com/auth/cloud-platform"];
        let config = google_cloud_auth::project::Config::default().with_scopes(&scopes);
        let provider = google_cloud_auth::token::DefaultTokenSourceProvider::new(config)
            .await
            .context("acquire Application Default Credentials for Vertex AI")?;
        Ok(Self {
            source: google_cloud_token::TokenSourceProvider::token_source(&provider),
        })
    }
}

#[async_trait]
impl TokenProvider for AdcToken {
    async fn token(&self) -> Result<String> {
        let raw = self
            .source
            .token()
            .await
            .map_err(|e| anyhow::anyhow!("ADC token: {e}"))?;
        Ok(raw.strip_prefix("Bearer ").unwrap_or(&raw).to_string())
    }
}

/// Claude Sonnet on Vertex AI. Construct with [`VertexProvider::new`] for
/// the defaults (global endpoint, the current Sonnet model, Sonnet list
/// rates) and adjust with the `with_*` builders; tests point
/// [`VertexProvider::with_endpoint_base`] at a mock server.
pub struct VertexProvider {
    /// GCP project that bills the completions.
    project: String,
    /// Endpoint region as it appears in the resource path. The default
    /// `global` serves the model off `aiplatform.googleapis.com`; a
    /// regional endpoint also swaps the host (see [`Self::with_region`]).
    region: String,
    /// Anthropic model id on the Vertex partner-model catalog.
    model_id: String,
    /// Completion budget the gateway asks for; the sandbox conversation
    /// is short-form drafting and research, not book-length generation.
    max_tokens: u32,
    /// Sonnet list rates, cents per million tokens ($2 / $10 per `MTok`).
    input_rate_cents_per_mtok: u64,
    output_rate_cents_per_mtok: u64,
    token: Arc<dyn TokenProvider>,
    http: reqwest::Client,
    /// Host root of the Agent Platform API. Overridable for mock-HTTP
    /// tests and for an egress proxy in front of VPC Service Controls.
    endpoint_base: String,
}

impl VertexProvider {
    /// A provider with the defaults: global endpoint, `claude-sonnet-5-5`
    /// (the Sonnet the glossary promises, retrieved from Anthropic's
    /// Vertex documentation this session), 4,096 completion tokens, and
    /// Sonnet list rates.
    #[must_use]
    pub fn new(project: impl Into<String>, token: Arc<dyn TokenProvider>) -> Self {
        Self {
            project: project.into(),
            region: "global".into(),
            model_id: "claude-sonnet-5-5".into(),
            max_tokens: 4_096,
            input_rate_cents_per_mtok: 200,
            output_rate_cents_per_mtok: 1_000,
            token,
            http: reqwest::Client::builder()
                // A hung completion must not hang a subscriber's session;
                // the wire calls a single `rawPredict` exchange.
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client with rustls tls builds"),
            endpoint_base: "https://aiplatform.googleapis.com".into(),
        }
    }

    /// Pin the model id (e.g. when the catalog moves past Sonnet 5.5).
    #[must_use]
    pub fn with_model_id(mut self, model_id: impl Into<String>) -> Self {
        self.model_id = model_id.into();
        self
    }

    /// Serve from a named region. A regional Vertex endpoint also lives
    /// on a regional host — set [`Self::with_endpoint_base`] to match
    /// (e.g. `https://us-east5-aiplatform.googleapis.com`).
    #[must_use]
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Override the API host root — a mock server in tests, or an egress
    /// proxy where VPC Service Controls front the network.
    #[must_use]
    pub fn with_endpoint_base(mut self, endpoint_base: impl Into<String>) -> Self {
        self.endpoint_base = endpoint_base.into();
        self
    }

    /// The `:rawPredict` URL for one completion. The model id rides the
    /// URL; the body never names it.
    #[must_use]
    pub fn endpoint_url(&self) -> String {
        format!(
            "{}/v1/projects/{}/locations/{}/publishers/anthropic/models/{}:rawPredict",
            self.endpoint_base, self.project, self.region, self.model_id
        )
    }

    /// The Anthropic Messages payload for a sandbox completion: the
    /// pinned `anthropic_version`, the completion budget, the
    /// conversation, the rendered skills as `system`, and the tool
    /// catalog. Pure — unit-tested without a network.
    #[must_use]
    pub fn build_request_body(&self, req: &CompletionRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = req
            .messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": m.content,
                })
            })
            .collect();
        let mut body = serde_json::json!({
            "anthropic_version": "vertex-2023-10-16",
            "max_tokens": self.max_tokens,
            "messages": messages,
        });
        let system = sandbox_skills::render_system_context(&req.active_skills);
        if !system.is_empty() {
            body["system"] = serde_json::Value::String(system);
        }
        if !req.tools.is_empty() {
            body["tools"] = serde_json::json!(req
                .tools
                .iter()
                .map(|t| serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                }))
                .collect::<Vec<_>>());
        }
        body
    }

    /// Split a Vertex reply into its reply text and its tool calls. Text
    /// blocks concatenate in order; `tool_use` blocks surface as
    /// [`crate::ToolCall`]s for the session loop to execute.
    fn reply_parts(content: &[VertexContentBlock]) -> Result<(String, Vec<crate::ToolCall>)> {
        let text: String = content
            .iter()
            .filter(|b| b.kind == "text")
            .filter_map(|b| b.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        let tool_calls: Vec<crate::ToolCall> = content
            .iter()
            .filter(|b| b.kind == "tool_use")
            .filter_map(|b| {
                b.name.clone().map(|name| crate::ToolCall {
                    id: b.id.clone().unwrap_or_default(),
                    name,
                    input: b.input.clone().unwrap_or(serde_json::Value::Null),
                })
            })
            .collect();
        if text.is_empty() && tool_calls.is_empty() {
            bail!("vertex response carried no text content and no tool calls");
        }
        Ok((text, tool_calls))
    }
}

/// One content block of a Vertex reply. Anthropic names the kind field
/// `type`, which is reserved in Rust. Text blocks carry a payload in
/// `text`; `tool_use` blocks name the catalog tool in `name` and carry
/// the call's arguments in `input`.
#[derive(Debug, Deserialize)]
struct VertexContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
}

/// Token usage as the provider reports it. Both fields are required — a
/// response without usage fails the completion rather than inventing a
/// zero (billing a silent zero is how a meter stops being honest).
#[derive(Debug, Deserialize)]
struct VertexUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct VertexResponse {
    content: Vec<VertexContentBlock>,
    usage: VertexUsage,
}

#[async_trait]
impl ModelProvider for VertexProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResult> {
        if req.messages.is_empty() {
            bail!("a completion needs a conversation; the request carried none");
        }
        let body = self.build_request_body(&req);
        let token = self
            .token
            .token()
            .await
            .context("mint Vertex AI bearer token")?;
        let response = self
            .http
            .post(self.endpoint_url())
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("vertex rawPredict request")?;
        let status = response.status();
        if !status.is_success() {
            bail!("vertex rawPredict returned {status}");
        }
        let body = response.text().await.context("read vertex response body")?;
        let parsed: VertexResponse = serde_json::from_str(&body).map_err(|error| {
            anyhow::anyhow!(
                "parse vertex response at line {}, column {}",
                error.line(),
                error.column()
            )
        })?;
        let (content, tool_calls) = VertexProvider::reply_parts(&parsed.content)?;
        let mut result = CompletionResult {
            messages: Vec::new(),
            tool_calls,
            input_tokens: parsed.usage.input_tokens,
            output_tokens: parsed.usage.output_tokens,
        };
        // A tool-call-only reply carries no text: sending an empty
        // assistant turn would ask the next request to quote nothing.
        if !content.is_empty() {
            result.messages.push(crate::Message {
                role: Role::Assistant,
                content,
            });
        }
        Ok(result)
    }

    fn price_cents(&self, usage: &CompletionResult) -> u64 {
        tokens_to_cents(
            usage.input_tokens,
            self.input_rate_cents_per_mtok,
            usage.output_tokens,
            self.output_rate_cents_per_mtok,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{StaticToken, VertexProvider};
    use crate::{CompletionRequest, CompletionResult, Message, ModelProvider, Role, SkillId};

    use serde_json::json;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn a_provider(base: String) -> VertexProvider {
        VertexProvider::new(
            "example-project",
            std::sync::Arc::new(StaticToken("test-token".into())),
        )
        .with_endpoint_base(base)
    }

    fn a_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Can my landlord raise rent mid-lease?".into(),
            }],
            ..CompletionRequest::default()
        }
    }

    fn a_reply() -> serde_json::Value {
        json!({
            "content": [{"type": "text", "text": "Generally no — the fixed term binds both parties."}],
            "usage": {"input_tokens": 12, "output_tokens": 34}
        })
    }

    #[tokio::test]
    async fn the_completion_posts_an_anthropic_messages_payload_and_maps_the_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/v1/projects/example-project/locations/global/publishers/anthropic/models/claude-sonnet-5-5:rawPredict",
            ))
            .and(body_string_contains("vertex-2023-10-16"))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_reply()))
            .expect(1)
            .mount(&server)
            .await;

        let request = CompletionRequest {
            active_skills: vec![SkillId::new("draft")],
            tools: vec![crate::ToolSpec {
                name: "search_case_law".into(),
                description: "Search case law".into(),
                input_schema: json!({"type": "object", "properties": {}}),
            }],
            ..a_request()
        };

        let result = a_provider(server.uri()).complete(request).await.unwrap();
        assert_eq!(
            result.messages,
            vec![Message {
                role: Role::Assistant,
                content: "Generally no — the fixed term binds both parties.".into(),
            }]
        );
        assert_eq!(result.input_tokens, 12);
        assert_eq!(result.output_tokens, 34);
        // 12 in / 34 out at the Sonnet rates is 36,400 micro-cents —
        // rounds up to the meter's minimum, one cent.
        assert_eq!(provider_priced_cents(&result), 1);
    }

    /// The price of a result at the provider's default Sonnet rates.
    fn provider_priced_cents(result: &CompletionResult) -> u64 {
        let provider = VertexProvider::new(
            "example-project",
            std::sync::Arc::new(StaticToken("t".into())),
        );
        provider.price_cents(result)
    }

    #[tokio::test]
    async fn the_wire_request_carries_skills_system_tools_and_mapped_roles() {
        let server = MockServer::start().await;
        let request = CompletionRequest {
            messages: vec![
                Message {
                    role: Role::User,
                    content: "Draft a demand letter.".into(),
                },
                Message {
                    role: Role::Assistant,
                    content: "Here is a first draft.".into(),
                },
                Message {
                    role: Role::User,
                    content: "Make it firmer.".into(),
                },
            ],
            active_skills: vec![SkillId::new("draft")],
            tools: vec![crate::ToolSpec {
                name: "search_legal_aid".into(),
                description: "Search legal aid resources".into(),
                input_schema: json!({"type": "object"}),
            }],
        };

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_reply()))
            .expect(1)
            .mount(&server)
            .await;

        let provider = a_provider(server.uri());
        provider.complete(request).await.unwrap();

        let sent = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
        assert_eq!(body["max_tokens"], 4_096);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "Draft a demand letter.");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["role"], "user");
        // The `draft` skill renders through the sandbox-skills catalog:
        // the vendored markdown, wrapped with the skill's header.
        assert!(body["system"]
            .as_str()
            .unwrap()
            .starts_with("# Legal Sandbox skill: "));
        assert_eq!(body["tools"][0]["name"], "search_legal_aid");
        assert_eq!(body["tools"][0]["input_schema"], json!({"type": "object"}));
        assert_eq!(sent.headers["authorization"], "Bearer test-token");
        assert!(sent.url.as_str().ends_with(":rawPredict"));
        // The model id rides the URL, never the body.
        assert!(body.get("model").is_none());
    }

    #[tokio::test]
    async fn a_session_without_resolvable_skills_omits_the_system_block() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(a_reply()))
            .mount(&server)
            .await;
        // The skill id resolves against the sandbox-skills catalog — an
        // unknown id degrades to no system block, never an error.
        let request = CompletionRequest {
            active_skills: vec![SkillId::new("no-such-skill")],
            ..a_request()
        };
        a_provider(server.uri()).complete(request).await.unwrap();

        let sent = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&sent.body).unwrap();
        assert!(body.get("system").is_none());
    }

    #[tokio::test]
    async fn a_non_2xx_from_vertex_is_an_error_that_names_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429).set_body_string(r#"{"error": "quota exceeded"}"#),
            )
            .mount(&server)
            .await;

        let error = a_provider(server.uri())
            .complete(a_request())
            .await
            .expect_err("a 429 fails the completion");
        assert!(error.to_string().contains("429"));
        assert!(!format!("{error:#}").contains("quota exceeded"));
    }

    #[tokio::test]
    async fn invalid_response_errors_do_not_disclose_provider_content() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "synthetic-sensitive-response"}],
                "usage": {"input_tokens": "synthetic-sensitive-response", "output_tokens": 1}
            })))
            .mount(&server)
            .await;

        let error = a_provider(server.uri())
            .complete(a_request())
            .await
            .expect_err("invalid usage fails the completion");
        assert!(error.to_string().contains("parse vertex response"));
        assert!(!format!("{error:#}").contains("synthetic-sensitive-response"));
    }

    #[tokio::test]
    async fn a_tool_use_reply_is_surfaced_not_dropped() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "search_case_law",
                    "input": {"query": "rent increase mid-lease"}
                }],
                "usage": {"input_tokens": 10, "output_tokens": 20}
            })))
            .mount(&server)
            .await;

        let result = a_provider(server.uri())
            .complete(a_request())
            .await
            .expect("a tool_use reply is surfaced, not dropped");
        assert_eq!(
            result.tool_calls,
            vec![crate::ToolCall {
                id: "toolu_1".into(),
                name: "search_case_law".into(),
                input: json!({"query": "rent increase mid-lease"}),
            }]
        );
        // No text block beside the call, so no empty assistant turn.
        assert!(result.messages.is_empty());
        assert_eq!(result.input_tokens, 10);
        assert_eq!(result.output_tokens, 20);
    }

    #[tokio::test]
    async fn a_mixed_text_and_tool_use_reply_carries_both() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [
                    {"type": "text", "text": "Let me look that up."},
                    {
                        "type": "tool_use",
                        "id": "toolu_2",
                        "name": "search_legal_aid",
                        "input": {"query": "eviction help Nevada"}
                    }
                ],
                "usage": {"input_tokens": 8, "output_tokens": 16}
            })))
            .mount(&server)
            .await;

        let result = a_provider(server.uri())
            .complete(a_request())
            .await
            .expect("text and tool calls coexist");
        assert_eq!(
            result.messages,
            vec![Message {
                role: Role::Assistant,
                content: "Let me look that up.".into(),
            }]
        );
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "search_legal_aid");
    }

    #[test]
    fn sonnet_usage_prices_at_the_sonnet_list_rates() {
        let provider = VertexProvider::new(
            "example-project",
            std::sync::Arc::new(StaticToken("t".into())),
        );
        // 1M input + 0.5M output at $2/$10 per MTok = $2 + $5 = 700¢.
        let usage = CompletionResult {
            messages: vec![],
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            tool_calls: Vec::new(),
        };
        assert_eq!(provider.price_cents(&usage), 700);
        // A realistic short turn: 12k in / 2k out = 2.4M + 2M micro-cents
        // → 5¢.
        let turn = CompletionResult {
            messages: vec![],
            input_tokens: 12_000,
            output_tokens: 2_000,
            tool_calls: Vec::new(),
        };
        assert_eq!(provider.price_cents(&turn), 5);
    }

    #[test]
    fn the_endpoint_url_pins_project_region_and_model() {
        let provider = VertexProvider::new(
            "example-project",
            std::sync::Arc::new(StaticToken("t".into())),
        );
        assert_eq!(
            provider.endpoint_url(),
            "https://aiplatform.googleapis.com/v1/projects/example-project/locations/global/publishers/anthropic/models/claude-sonnet-5-5:rawPredict"
        );
        let regional = provider
            .with_region("us-east5")
            .with_endpoint_base("https://us-east5-aiplatform.googleapis.com");
        assert_eq!(
            regional.endpoint_url(),
            "https://us-east5-aiplatform.googleapis.com/v1/projects/example-project/locations/us-east5/publishers/anthropic/models/claude-sonnet-5-5:rawPredict"
        );
    }
}
