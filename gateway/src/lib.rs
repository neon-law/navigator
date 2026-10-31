//! Model-provider seam for the [Legal Sandbox](https://www.neonlaw.com/glossary/legal-sandbox).
//!
//! The sandbox owes every Nautilus subscriber $5 of metered model usage a
//! month, priced à la carte beyond the credit. Metering that honestly
//! needs one contract every backend answers, regardless of who serves the
//! tokens — that is [`ModelProvider`]. The shipped backends:
//!
//! - [`VertexProvider`] — Claude Sonnet on GCP Vertex AI, the production
//!   backend (production auth is Application Default Credentials →
//!   Workload Identity).
//! - [`StubModelProvider`] — a deterministic in-process stub for dev and
//!   tests, following the `billing` crate's stub pattern: fixed token
//!   counts, an injectable failure mode, and every call recorded so tests
//!   can assert the exchange.
//!
//! A zero-cloud local backend (Ollama serving open weights) is planned
//! behind the same trait; the trait, not the backend list, is the
//! contract the meter and the sandbox session build on.
//!
//! Session skills are the [Sandbox Skill](https://www.neonlaw.com/glossary/sandbox-skill)
//! catalog: the gateway renders each requested skill's vendored
//! instructions into the system context through the `sandbox-skills`
//! crate and re-exports [`render_system_context`] and [`SkillId`] so
//! callers need only this crate.
//!
//! Two house rules shape the types:
//!
//! - **Costs are integer cents, never floats** — the same rule `billing`
//!   enforces, so [`ModelProvider::price_cents`] feeds the usage ledger
//!   without a money conversion.
//! - **No identity in the payload** — a request carries the sanitized
//!   conversation, the session's skills, and the tool catalog, and
//!   nothing else. Sanitization is the caller's job (the sandbox session
//!   owns it); the gateway never receives a person id to leak.

pub mod metered;
pub mod stub;
pub mod vertex;

pub use metered::{MeteredError, MeteredOutcome, MeteredSession, StoreLedger, UsageLedger};
pub use stub::StubModelProvider;
pub use vertex::{adc_token_provider, StaticToken, TokenProvider, VertexProvider};

pub use sandbox_skills::{catalog, render_system_context, SkillId};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Who authored a [`Message`] in a sandbox exchange. The two visible
/// kinds of an [Assistant Turn](https://www.neonlaw.com/glossary/assistant-turn)
/// — a subscriber's prompt and the assistant's reply; tool calls and
/// their results are the session loop's records, not gateway messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// One message of a sandbox conversation: a [`Role`] and its plain text.
/// The gateway transports text; richer content (tool calls, citations) is
/// the session layer's business and stays out of the seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// One tool the model may call — a Navigator MCP catalog entry: its name,
/// what it does, and the JSON Schema of its input. Rendered into the
/// model request; executing what the model asks for is the sandbox
/// session loop's job, not the gateway's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// One sandbox completion request: the subscriber-visible conversation
/// history (already sanitized — no identifiers beyond the session's own
/// person id), the skills loaded for this session, and the tools the
/// model may call. Serializable so the session can journal what it asked
/// for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub messages: Vec<Message>,
    pub active_skills: Vec<SkillId>,
    pub tools: Vec<ToolSpec>,
}

/// One tool call the model asked for: which catalog tool, its input,
/// and the provider's call id. The gateway only surfaces it — executing
/// the call is the sandbox session loop's job (the tool entries are
/// read-only Navigator MCP search tools, executed behind the session's
/// authorization). Serializable so the transcript can pair the call to
/// its result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for this call — the transcript pairs a `tool_use`
    /// block to the `tool_result` that answers it by this id.
    pub id: String,
    /// The catalog tool's name, e.g. `search_case_law`.
    pub name: String,
    /// The call's arguments, as the model produced them.
    pub input: serde_json::Value,
}

/// One sandbox completion: what the model replied plus the usage it
/// reported, in tokens. A reply may ask for tool calls instead of (or
/// alongside) final text — [`ToolCall`]s surface here for the session
/// loop to execute and re-prompt with. Costs are derived from this usage
/// by [`ModelProvider::price_cents`] — a failed completion returns `Err`,
/// carries no usage, and is never priced. Serializable so the meter can
/// journal the usage it debits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CompletionResult {
    pub messages: Vec<Message>,
    /// Tool calls the model asked for, in the order it asked for them.
    /// Empty for a plain text reply.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// The seam that makes Vertex and local interchangeable: one sandbox
/// completion, priced in cents. Backends are `Send + Sync` so the portal
/// can hold one behind an `Arc` and share it across sessions.
///
/// There is deliberately no retry policy on the trait: a provider that
/// fails returns `Err` and reports no usage, and the caller — the
/// metered sandbox session — decides what a retry costs.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    /// Run one completion. `Ok` reports the usage the provider charged;
    /// `Err` is a failed completion and must never be billed.
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResult>;

    /// Cost of the reported usage in integer cents, at the provider's
    /// current rate.
    fn price_cents(&self, usage: &CompletionResult) -> u64;
}

/// Token usage at the given rates, in integer cents. Rates are cents per
/// million tokens (e.g. Claude Sonnet 5.5 lists $2 input / $10 output per
/// `MTok` → 200 and 1,000); the product is summed and rounded **up** to the
/// whole cent, so a sub-cent completion still registers on the meter
/// instead of riding free between whole cents. The arithmetic is u128 —
/// a u64 token count times a u64 rate overflows u64 long before any
/// realistic usage, and saturates at `u64::MAX` beyond it.
#[must_use]
pub fn tokens_to_cents(
    input_tokens: u64,
    input_rate_cents_per_mtok: u64,
    output_tokens: u64,
    output_rate_cents_per_mtok: u64,
) -> u64 {
    let micro_cents = u128::from(input_tokens) * u128::from(input_rate_cents_per_mtok)
        + u128::from(output_tokens) * u128::from(output_rate_cents_per_mtok);
    let cents = micro_cents.div_ceil(1_000_000);
    u64::try_from(cents).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        tokens_to_cents, CompletionRequest, CompletionResult, Message, Role, SkillId, ToolSpec,
    };
    use serde_json::json;

    #[test]
    fn a_role_serializes_as_the_model_apis_lowercase_word() {
        assert_eq!(serde_json::to_value(Role::User).unwrap(), json!("user"));
        assert_eq!(
            serde_json::to_value(Role::Assistant).unwrap(),
            json!("assistant")
        );
    }

    #[test]
    fn a_request_and_result_round_trip_through_serde() {
        let request = CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Is my lease assignable?".into(),
            }],
            active_skills: vec![SkillId::new("drafting")],
            tools: vec![ToolSpec {
                name: "search_case_law".into(),
                description: "Search case law".into(),
                input_schema: json!({"type": "object"}),
            }],
        };
        let back: CompletionRequest =
            serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
        assert_eq!(back, request);

        let result = CompletionResult {
            messages: vec![Message {
                role: Role::Assistant,
                content: "Generally, no — but check the clause.".into(),
            }],
            input_tokens: 12,
            output_tokens: 34,
            tool_calls: Vec::new(),
        };
        let back: CompletionResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(back, result);
    }

    #[test]
    fn token_usage_prices_in_whole_cents_rounded_up() {
        // Exact division: 1M input at 200 ¢/MTok is exactly 200¢.
        assert_eq!(tokens_to_cents(1_000_000, 200, 0, 0), 200);
        // Sub-cent usage rounds up so the meter never rides free between
        // whole cents: 1 token at 200 ¢/MTok is 0.0002¢ → 1¢.
        assert_eq!(tokens_to_cents(1, 200, 0, 0), 1);
        // Zero usage costs nothing.
        assert_eq!(tokens_to_cents(0, 200, 0, 1_000), 0);
        // A realistic sandbox turn at Sonnet 5.5 rates: 12k in, 2k out →
        // 12_000·200 + 2_000·1_000 = 2.4M + 2M micro-cents → ceil 5¢.
        assert_eq!(tokens_to_cents(12_000, 200, 2_000, 1_000), 5);
        // The sum, not each leg, rounds: 600_000 in (120¢ exact) plus
        // 1 out (negligible) stays 120¢, but 600_000 in + 600_000 out at
        // 200/1_000 = 120¢ + 600¢ = 720¢ exactly.
        assert_eq!(tokens_to_cents(600_000, 200, 600_000, 1_000), 720);
        // Huge counts saturate instead of overflowing.
        assert_eq!(tokens_to_cents(u64::MAX, u64::MAX, 0, 0), u64::MAX);
    }
}
