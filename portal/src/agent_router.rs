//! Natural-language → tool routing, for the A2A handler and the
//! Assistant Chat.
//!
//! A2A's design treats agents as opaque message handlers: the client
//! sends a freeform user message via `message/send` and the agent
//! decides which of its declared skills to invoke. This module owns
//! the seam where that decision would be made: the [`AgentRouter`]
//! trait takes the user's text plus the MCP tool descriptors and
//! returns a [`RoutedCall`] naming a specific tool and its arguments.
//! The A2A handler dispatches the returned call through the same
//! `mcp::tools::call_tool` that direct `metadata.skill` calls use, so
//! a routing decision is the *only* new code path a provider adds.
//!
//! Two implementations ship. [`NullRouter`] always returns
//! [`RouterError::NotConfigured`]; it is the A2A handler's router, which
//! turns that into a Task pointing the caller at `metadata.skill`.
//! [`crate::claude_router::ClaudeVertexRouter`] is Claude on Vertex AI,
//! for the Assistant Chat's read-only loop, [`run_tool_loop`].
//!
//! The seam is the invariant: a provider (a local model, a rules
//! engine) is a new `impl AgentRouter` chosen from `lib.rs`, and it
//! reuses this tool catalog rather than forking one. The A2A handler never knows which
//! implementation it holds.

use async_trait::async_trait;
use serde_json::Value;

/// What the router picked. The tool name is the MCP tool name, which
/// is also the A2A skill id — one bare string, dispatched as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedCall {
    pub tool_name: String,
    pub arguments: Value,
    /// The provider's id for this call. The call's [`Turn::Result`] carries
    /// the same id so the provider can pair them; empty when the provider
    /// has none.
    pub call_id: String,
    /// What the provider needs echoed back to accept this call's result —
    /// for Claude, the assistant turn's content blocks verbatim, thinking
    /// signatures included. Opaque to everything but the router that set it.
    pub provider_turn: Option<Value>,
}

impl RoutedCall {
    /// A call named directly rather than chosen by a provider: no call id,
    /// nothing to echo.
    #[must_use]
    pub fn named(tool_name: impl Into<String>, arguments: Value) -> Self {
        Self {
            tool_name: tool_name.into(),
            arguments,
            call_id: String::new(),
            provider_turn: None,
        }
    }
}

/// One entry in the router's running view of the conversation. The
/// A2A handler owns this history and grows it a step at a time; the
/// router only reads it to decide the next move. Kept
/// provider-neutral: an implementation maps these onto whatever
/// transcript shape its own model expects.
#[derive(Debug, Clone)]
pub enum Turn {
    /// The human's free-form request. Always the first entry.
    User(String),
    /// A tool the router chose and the handler then executed this round.
    Call(RoutedCall),
    /// The result that call returned, fed back to the model so it can
    /// decide the next step (or finish). `content` is the tool's MCP
    /// result payload, or an `{ "error": … }` object when the call
    /// failed — feeding failures back lets the model self-correct
    /// (e.g. a `NotFound` nudges it to look the person up differently)
    /// rather than the loop dead-ending on the first miss.
    Result {
        call_id: String,
        tool_name: String,
        content: Value,
    },
}

impl Turn {
    /// A call and the result it produced, in history order.
    #[must_use]
    pub fn exchange(call: RoutedCall, content: Value) -> [Self; 2] {
        let result = Self::Result {
            call_id: call.call_id.clone(),
            tool_name: call.tool_name.clone(),
            content,
        };
        [Self::Call(call), result]
    }
}

/// What the router decided to do next, given the history so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Execute this call, append its result to the history, ask again.
    Call(RoutedCall),
    /// No more tools — Navigator MCP's final word. May be empty when the model
    /// stops without commentary.
    Done(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// No router configured (KIND / local dev). Surfaced to A2A
    /// callers as a Task with a helpful text part — *not* a JSON-RPC
    /// error envelope.
    #[error("agent router not configured (set NAVIGATOR_GCP_PROJECT_ID + NAVIGATOR_GCP_LOCATION to enable Vertex AI routing)")]
    NotConfigured,
    /// Router answered but couldn't pick any skill for the input.
    #[error("router could not pick a skill for: {0}")]
    NoMatch(String),
    /// Transport / HTTP layer failure (metadata server, Vertex AI).
    #[error("router transport: {0}")]
    Transport(String),
    /// Router returned a malformed response we couldn't parse.
    #[error("router returned invalid response: {0}")]
    InvalidResponse(String),
}

#[async_trait]
pub trait AgentRouter: Send + Sync {
    /// Decide the next step given the conversation so far. The first
    /// call passes `[Turn::User(text)]`; the handler then appends the
    /// chosen call and its result and calls again, until the router
    /// answers [`Step::Done`]. `skills` is the same list of MCP
    /// descriptors `mcp::tools::list_tools()` returns — each has
    /// `name`, `description`, `inputSchema`.
    ///
    /// This is the agentic loop's single LLM hop. Keeping it one step
    /// (rather than owning the whole loop) leaves tool execution — and
    /// thus the DB — entirely on the handler side, so the router stays
    /// a pure brain with no `McpState` dependency.
    async fn next_step(&self, history: &[Turn], skills: &[Value]) -> Result<Step, RouterError>;
}

// ---------------------------------------------------------------------------
// run_tool_loop — the read-only loop an Assistant Chat runs.
// ---------------------------------------------------------------------------

/// A finished loop: the router's final text and the whole history, which
/// the caller stores as the chat's turns.
#[derive(Debug, Clone)]
pub struct ToolLoopOutcome {
    pub text: String,
    pub history: Vec<Turn>,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolLoopError {
    #[error(transparent)]
    Router(#[from] RouterError),
    /// The router kept asking for tools past the cap.
    #[error("the assistant did not finish within {0} steps")]
    StepLimit(usize),
}

/// Ask `router` for steps until it is done, running each call through
/// `call` and feeding the result back.
///
/// Only a tool named in `tools` is ever dispatched: any other name is
/// answered with an error result the model can read, and `call` never sees
/// it. Unlike the A2A loop there is no confirmation pause, because the
/// caller offers read-only tools only. `max_steps` bounds the number of
/// router hops, so one prompt cannot loop forever.
pub async fn run_tool_loop<F, Fut>(
    router: &dyn AgentRouter,
    mut history: Vec<Turn>,
    tools: &[Value],
    max_steps: usize,
    mut call: F,
) -> Result<ToolLoopOutcome, ToolLoopError>
where
    F: FnMut(String, Value) -> Fut,
    Fut: std::future::Future<Output = Value>,
{
    for _ in 0..max_steps {
        match router.next_step(&history, tools).await? {
            Step::Done(text) => return Ok(ToolLoopOutcome { text, history }),
            Step::Call(chosen) => {
                let offered = tools
                    .iter()
                    .any(|t| t["name"].as_str() == Some(chosen.tool_name.as_str()));
                let content = if offered {
                    call(chosen.tool_name.clone(), chosen.arguments.clone()).await
                } else {
                    serde_json::json!({ "error": "that tool was not offered to this chat" })
                };
                history.extend(Turn::exchange(chosen, content));
            }
        }
    }
    Err(ToolLoopError::StepLimit(max_steps))
}

// ---------------------------------------------------------------------------
// NullRouter — used when the env hasn't been configured (KIND / tests
// that don't exercise the router path).
// ---------------------------------------------------------------------------

/// No-op router. Always returns `NotConfigured`.
#[derive(Debug, Default)]
pub struct NullRouter;

#[async_trait]
impl AgentRouter for NullRouter {
    async fn next_step(&self, _history: &[Turn], _skills: &[Value]) -> Result<Step, RouterError> {
        Err(RouterError::NotConfigured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn glossary_tools() -> Vec<Value> {
        vec![json!({ "name": "glossary_show", "description": "", "inputSchema": {} })]
    }

    /// Asks for `tool` until it has `answers` results, then finishes.
    struct Scripted {
        tool: &'static str,
        answers: usize,
    }

    #[async_trait]
    impl AgentRouter for Scripted {
        async fn next_step(&self, history: &[Turn], _: &[Value]) -> Result<Step, RouterError> {
            let results = history
                .iter()
                .filter(|t| matches!(t, Turn::Result { .. }))
                .count();
            if results >= self.answers {
                return Ok(Step::Done(format!("done after {results}")));
            }
            Ok(Step::Call(RoutedCall {
                tool_name: self.tool.into(),
                arguments: json!({ "term": "Matter" }),
                call_id: format!("call_{results}"),
                provider_turn: None,
            }))
        }
    }

    #[tokio::test]
    async fn the_loop_runs_an_offered_tool_and_pairs_its_result_by_call_id() {
        let calls = Mutex::new(Vec::new());
        let outcome = run_tool_loop(
            &Scripted {
                tool: "glossary_show",
                answers: 1,
            },
            vec![Turn::User("What is a Matter?".into())],
            &glossary_tools(),
            6,
            |name, arguments| {
                calls.lock().unwrap().push((name, arguments));
                async { json!({ "content": [{ "type": "text", "text": "A Matter is…" }] }) }
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.text, "done after 1");
        assert_eq!(
            *calls.lock().unwrap(),
            [("glossary_show".to_string(), json!({ "term": "Matter" }))]
        );
        assert!(matches!(
            &outcome.history[2],
            Turn::Result { call_id, tool_name, .. } if call_id == "call_0" && tool_name == "glossary_show"
        ));
        assert_eq!(outcome.history.len(), 3);
    }

    #[tokio::test]
    async fn the_loop_stops_at_its_step_cap() {
        let error = run_tool_loop(
            &Scripted {
                tool: "glossary_show",
                answers: usize::MAX,
            },
            vec![Turn::User("loop forever".into())],
            &glossary_tools(),
            3,
            |_, _| async { json!({}) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ToolLoopError::StepLimit(3)), "{error}");
    }

    #[tokio::test]
    async fn a_tool_that_was_not_offered_is_refused_and_never_dispatched() {
        let dispatched = Mutex::new(0);
        let outcome = run_tool_loop(
            &Scripted {
                tool: "create_person",
                answers: 1,
            },
            vec![Turn::User("add someone".into())],
            &glossary_tools(),
            6,
            |_, _| {
                *dispatched.lock().unwrap() += 1;
                async { json!({}) }
            },
        )
        .await
        .unwrap();
        assert_eq!(*dispatched.lock().unwrap(), 0);
        let Turn::Result { content, .. } = &outcome.history[2] else {
            panic!("the refusal is fed back as the call's result");
        };
        assert!(content["error"].as_str().unwrap().contains("not offered"));
    }

    #[tokio::test]
    async fn a_router_error_ends_the_loop() {
        let error = run_tool_loop(
            &NullRouter,
            vec![Turn::User("hi".into())],
            &glossary_tools(),
            6,
            |_, _| async { json!({}) },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ToolLoopError::Router(RouterError::NotConfigured)
        ));
    }
}
