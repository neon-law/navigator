//! [`ClaudeVertexRouter`] — Claude on Vertex AI as an [`AgentRouter`].
//!
//! One router hop is one Messages request: the provider-neutral [`Turn`]
//! history becomes Claude messages, the MCP descriptors become Claude tool
//! definitions, and the reply becomes a [`Step`]. The tool loop itself stays
//! in [`crate::agent_router::run_tool_loop`] (Assistant Chat) or the A2A
//! handler, so this module never executes a tool.
//!
//! A reply that calls a tool is carried in [`RoutedCall::provider_turn`]
//! verbatim and echoed as the next request's assistant message: Claude
//! accepts a thinking block only exactly as it produced it. Parallel tool
//! use is turned off, because a loop answers one call per hop and every
//! `tool_use` in a reply must be answered.

use async_trait::async_trait;
use cloud::{ClaudeChatRequest, ClaudeMessage, ClaudeToolResult, ClaudeVertexAdapter, VertexError};
use serde_json::Value;

use crate::agent_router::{AgentRouter, RoutedCall, RouterError, Step, Turn};

/// The standing instruction for the Assistant Chat.
const SYSTEM_PROMPT: &str = "You are the assistant inside Neon Law Navigator, talking with a \
     signed-in person. Use Navigator's tools to ground answers in the firm's own vocabulary: \
     look terms up with glossary_list and glossary_show rather than guessing what a Navigator \
     word means. You give general information, not legal advice, and you say so when a question \
     calls for a lawyer's judgment.";

/// What the chat shows when the model declines a request.
pub const REFUSAL_TEXT: &str = "I can't help with that request.";

/// Output budget for one non-streaming reply.
const MAX_TOKENS: u32 = 16_000;

/// Claude Messages on Vertex AI, one request per router hop.
pub struct ClaudeVertexRouter {
    adapter: ClaudeVertexAdapter,
    project_id: String,
    location: String,
    model: String,
}

impl ClaudeVertexRouter {
    #[must_use]
    pub fn new(
        adapter: ClaudeVertexAdapter,
        project_id: impl Into<String>,
        location: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            adapter,
            project_id: project_id.into(),
            location: location.into(),
            model: model.into(),
        }
    }
}

#[async_trait]
impl AgentRouter for ClaudeVertexRouter {
    async fn next_step(&self, history: &[Turn], skills: &[Value]) -> Result<Step, RouterError> {
        let request = ClaudeChatRequest {
            project_id: self.project_id.clone(),
            model: self.model.clone(),
            location: self.location.clone(),
            system: SYSTEM_PROMPT.to_string(),
            messages: messages_for(history),
            tools: skills.iter().map(claude_tool).collect(),
            max_tokens: MAX_TOKENS,
            one_tool_per_turn: true,
        };
        let reply = self.adapter.send(&request).await.map_err(router_error)?;
        if reply.stop_reason.as_deref() == Some("refusal") {
            return Ok(Step::Done(REFUSAL_TEXT.to_string()));
        }
        let mut uses = reply.tool_uses().into_iter();
        let Some(tool_use) = uses.next() else {
            return Ok(Step::Done(reply.text()));
        };
        if uses.next().is_some() {
            return Err(RouterError::InvalidResponse(
                "more than one tool call in one turn".into(),
            ));
        }
        Ok(Step::Call(RoutedCall {
            tool_name: tool_use.name,
            arguments: tool_use.input,
            call_id: tool_use.id,
            provider_turn: Some(Value::Array(reply.content)),
        }))
    }
}

/// The history as Claude messages. A call without a provider turn (one
/// named directly) is rebuilt as a lone `tool_use` block.
fn messages_for(history: &[Turn]) -> Vec<ClaudeMessage> {
    history
        .iter()
        .map(|turn| match turn {
            Turn::User(text) => ClaudeMessage::user_text(text.clone()),
            Turn::Call(call) => ClaudeMessage::assistant(match &call.provider_turn {
                Some(Value::Array(blocks)) => blocks.clone(),
                _ => vec![serde_json::json!({
                    "type": "tool_use",
                    "id": call.call_id,
                    "name": call.tool_name,
                    "input": call.arguments,
                })],
            }),
            Turn::Result {
                call_id, content, ..
            } => ClaudeMessage::tool_results([tool_result(call_id, content)]),
        })
        .collect()
}

/// One MCP tool payload as a Claude tool result: its text parts, or the
/// `error` the loop recorded.
fn tool_result(call_id: &str, content: &Value) -> ClaudeToolResult {
    if let Some(error) = content["error"].as_str() {
        return ClaudeToolResult {
            tool_use_id: call_id.to_string(),
            content: error.to_string(),
            is_error: true,
        };
    }
    let text: Vec<&str> = content["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect();
    ClaudeToolResult {
        tool_use_id: call_id.to_string(),
        content: if text.is_empty() {
            content.to_string()
        } else {
            text.join("\n\n")
        },
        is_error: false,
    }
}

/// An MCP descriptor as a Claude tool definition.
fn claude_tool(descriptor: &Value) -> Value {
    serde_json::json!({
        "name": descriptor["name"],
        "description": descriptor["description"],
        "input_schema": descriptor["inputSchema"],
    })
}

/// A Vertex failure, named by kind only: [`VertexError`] never carries a
/// provider body, so neither does this.
fn router_error(error: VertexError) -> RouterError {
    match error {
        VertexError::InvalidResponse | VertexError::InvalidModelOutput => {
            RouterError::InvalidResponse(error.to_string())
        }
        other => RouterError::Transport(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_router::{run_tool_loop, Turn};
    use serde_json::json;
    use std::sync::Arc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PATH: &str =
        "/v1/projects/synthetic-project/locations/global/publishers/anthropic/models/claude-test:rawPredict";

    fn router(server: &MockServer) -> ClaudeVertexRouter {
        let adapter = cloud::ClaudeVertexAdapter::new(Arc::new(cloud::StaticTokenSource::new("t")))
            .unwrap()
            .with_base_url(server.uri());
        ClaudeVertexRouter::new(adapter, "synthetic-project", "global", "claude-test")
    }

    async fn glossary(name: String, arguments: Value) -> Value {
        match name.as_str() {
            "glossary_show" => mcp::tools::glossary_show::call(&arguments).await.unwrap(),
            other => panic!("unexpected tool {other}"),
        }
    }

    #[tokio::test]
    async fn sonnet_looks_up_the_glossary_then_answers_with_the_turn_echoed() {
        let server = MockServer::start().await;
        let first = json!([
            { "type": "thinking", "thinking": "", "signature": "opaque" },
            { "type": "tool_use", "id": "toolu_1", "name": "glossary_show", "input": { "term": "Matter" } }
        ]);
        Mock::given(method("POST"))
            .and(path(PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": first, "stop_reason": "tool_use"
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{ "type": "text", "text": "A Matter is an engagement." }],
                "stop_reason": "end_turn"
            })))
            .mount(&server)
            .await;

        let outcome = run_tool_loop(
            &router(&server),
            vec![Turn::User("What is a Matter?".into())],
            &mcp::tools::assistant_catalog(),
            6,
            glossary,
        )
        .await
        .unwrap();
        assert_eq!(outcome.text, "A Matter is an engagement.");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let first_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            first_body["tools"][0],
            json!({
                "name": "glossary_list",
                "description": mcp::tools::glossary_list::descriptor()["description"],
                "input_schema": mcp::tools::glossary_list::descriptor()["inputSchema"],
            })
        );
        assert_eq!(
            first_body["tool_choice"],
            json!({ "type": "auto", "disable_parallel_tool_use": true })
        );
        let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
        let messages = second["messages"].as_array().unwrap();
        assert_eq!(
            messages[1],
            json!({ "role": "assistant", "content": first })
        );
        let result = &messages[2]["content"][0];
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "toolu_1");
        let matter = store::glossary::find("Matter").unwrap();
        assert!(result["content"]
            .as_str()
            .unwrap()
            .contains(matter.body.trim()));
        assert!(result.get("is_error").is_none());
    }

    #[test]
    fn a_call_without_a_provider_turn_is_rebuilt_as_a_tool_use() {
        let history = vec![
            Turn::User("hi".into()),
            Turn::Call(RoutedCall {
                tool_name: "glossary_list".into(),
                arguments: json!({}),
                call_id: "toolu_9".into(),
                provider_turn: None,
            }),
            Turn::Result {
                call_id: "toolu_9".into(),
                tool_name: "glossary_list".into(),
                content: json!({ "error": "boom" }),
            },
        ];
        let messages = messages_for(&history);
        assert_eq!(
            messages[1].content,
            [json!({ "type": "tool_use", "id": "toolu_9", "name": "glossary_list", "input": {} })]
        );
        assert_eq!(
            messages[2].content,
            [
                json!({ "type": "tool_result", "tool_use_id": "toolu_9", "content": "boom", "is_error": true })
            ]
        );
    }

    #[tokio::test]
    async fn a_refusal_ends_the_chat_with_a_plain_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [], "stop_reason": "refusal"
            })))
            .mount(&server)
            .await;
        let step = router(&server)
            .next_step(&[Turn::User("hi".into())], &mcp::tools::assistant_catalog())
            .await
            .unwrap();
        assert_eq!(step, Step::Done(REFUSAL_TEXT.to_string()));
    }

    #[tokio::test]
    async fn more_than_one_tool_call_in_a_turn_is_an_invalid_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [
                    { "type": "tool_use", "id": "a", "name": "glossary_list", "input": {} },
                    { "type": "tool_use", "id": "b", "name": "glossary_list", "input": {} }
                ],
                "stop_reason": "tool_use"
            })))
            .mount(&server)
            .await;
        let error = router(&server)
            .next_step(&[Turn::User("hi".into())], &mcp::tools::assistant_catalog())
            .await
            .unwrap_err();
        assert!(matches!(error, RouterError::InvalidResponse(_)), "{error}");
    }

    #[tokio::test]
    async fn vertex_failures_surface_as_transport_errors_without_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503).set_body_string("private upstream detail"))
            .mount(&server)
            .await;
        let error = router(&server)
            .next_step(&[Turn::User("hi".into())], &mcp::tools::assistant_catalog())
            .await
            .unwrap_err();
        assert!(matches!(error, RouterError::Transport(_)), "{error}");
        assert!(!error.to_string().contains("private"));
    }
}
