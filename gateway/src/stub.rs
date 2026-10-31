//! Deterministic in-process [`ModelProvider`] for dev and tests — the
//! `billing` crate's stub pattern applied to model calls: fixed token
//! counts, an injectable failure mode, and every call recorded to an
//! internal Mutex so tests can assert the exchange fired with the right
//! request. It talks to no network and needs no credentials.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::{tokens_to_cents, CompletionRequest, CompletionResult, Message, ModelProvider, Role};

/// In-process stub model provider. Every successful completion answers
/// `Stub reply <seq>.` with the same fixed token counts, so usage-ledger
/// tests can price a known number of cents; the injected failure mode
/// makes every completion fail with that message instead. Failed
/// completions return `Err` and carry no usage — there is no result to
/// price, so nothing is ever debited for one.
pub struct StubModelProvider {
    /// Token counts every successful completion reports.
    input_tokens: u64,
    output_tokens: u64,
    /// `price_cents` rates, cents per million tokens. Defaulted to the
    /// Vertex Sonnet rates so stub and production prices agree unless a
    /// test overrides them.
    input_rate_cents_per_mtok: u64,
    output_rate_cents_per_mtok: u64,
    /// When set, every completion fails with this message.
    failure: Mutex<Option<String>>,
    /// Scripted tool-call rounds: each successful completion pops the
    /// front entry and reports those calls instead of none. An empty
    /// script means every completion is a plain text reply.
    tool_script: Mutex<Vec<Vec<crate::ToolCall>>>,
    /// Every `complete` call, successful or not, in call order.
    calls: Mutex<Vec<CompletionRequest>>,
}

impl Default for StubModelProvider {
    fn default() -> Self {
        Self {
            input_tokens: 1_000,
            output_tokens: 500,
            input_rate_cents_per_mtok: 200,
            output_rate_cents_per_mtok: 1_000,
            failure: Mutex::new(None),
            tool_script: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl StubModelProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the token counts a successful completion reports, so a
    /// test can price a chosen number of cents.
    pub fn set_usage(&mut self, input_tokens: u64, output_tokens: u64) {
        self.input_tokens = input_tokens;
        self.output_tokens = output_tokens;
    }

    /// Override the pricing rates (cents per million tokens).
    pub fn set_rates(&mut self, input_cents_per_mtok: u64, output_cents_per_mtok: u64) {
        self.input_rate_cents_per_mtok = input_cents_per_mtok;
        self.output_rate_cents_per_mtok = output_cents_per_mtok;
    }

    /// Inject a failure mode: every subsequent completion fails with this
    /// message. `None` clears it. Modeled on a backend outage — the
    /// shape the sandbox session must handle without billing anyone.
    pub fn set_failure(&self, message: Option<String>) {
        *self
            .failure
            .lock()
            .expect("stub provider failure-mode lock") = message;
    }

    /// Script one round of tool calls: the next successful completion
    /// reports them, the one after reports none (unless more rounds are
    /// queued). Models the search-tool loop deterministically — the
    /// session asks, the model answers with a call, the loop re-prompts.
    pub fn queue_tool_calls(&self, calls: Vec<crate::ToolCall>) {
        self.tool_script
            .lock()
            .expect("stub provider tool-script lock")
            .push(calls);
    }

    /// Snapshot of every call so far, in call order — successful or
    /// failed. Cheap clone; callers can hold onto it across `await`
    /// points.
    #[must_use]
    pub fn calls(&self) -> Vec<CompletionRequest> {
        self.calls.lock().expect("stub provider lock").clone()
    }
}

#[async_trait]
impl ModelProvider for StubModelProvider {
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<CompletionResult> {
        {
            let mut calls = self.calls.lock().expect("stub provider lock");
            calls.push(req);
            if let Some(message) = self
                .failure
                .lock()
                .expect("stub provider failure-mode lock")
                .as_ref()
            {
                // Fail before producing any usage: an `Err` carries no
                // `CompletionResult`, so there is nothing for the meter
                // to price — the failure-produces-no-usage property by
                // construction.
                return Err(anyhow::anyhow!("stub model provider: {message}"));
            }
            let seq = calls.len();
            let tool_calls = self
                .tool_script
                .lock()
                .expect("stub provider tool-script lock")
                .pop()
                .unwrap_or_default();
            Ok(CompletionResult {
                messages: vec![Message {
                    role: Role::Assistant,
                    content: format!("Stub reply {seq}."),
                }],
                tool_calls,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            })
        }
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
    use super::StubModelProvider;
    use crate::{CompletionRequest, Message, ModelProvider, Role};

    fn a_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message {
                role: Role::User,
                content: "What is a quiet title action?".into(),
            }],
            ..CompletionRequest::default()
        }
    }

    #[tokio::test]
    async fn a_completion_answers_deterministically_and_reports_its_fixed_usage() {
        let mut provider = StubModelProvider::new();
        provider.set_usage(1_000, 500);

        let first = provider.complete(a_request()).await.unwrap();
        assert_eq!(first.messages.len(), 1);
        assert_eq!(first.messages[0].role, Role::Assistant);
        assert_eq!(first.messages[0].content, "Stub reply 1.");
        assert_eq!(first.input_tokens, 1_000);
        assert_eq!(first.output_tokens, 500);

        // The second completion is the same shape, numbered forward.
        let second = provider.complete(a_request()).await.unwrap();
        assert_eq!(second.messages[0].content, "Stub reply 2.");
        assert_eq!(second.input_tokens, 1_000);
        assert_eq!(second.output_tokens, 500);
    }

    #[tokio::test]
    async fn every_call_is_recorded_so_tests_can_assert_the_request() {
        let mut provider = StubModelProvider::new();
        provider.set_usage(10, 2);
        let skills = vec![crate::SkillId::new("draft")];
        let request = CompletionRequest {
            active_skills: skills.clone(),
            ..a_request()
        };
        provider.complete(request.clone()).await.unwrap();

        let calls = provider.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].active_skills, skills);
        assert_eq!(calls[0].messages, request.messages);
    }

    #[tokio::test]
    async fn a_scripted_tool_call_round_is_emitted_once() {
        let mut provider = StubModelProvider::new();
        provider.queue_tool_calls(vec![crate::ToolCall {
            id: "toolu_1".into(),
            name: "search_case_law".into(),
            input: serde_json::json!({"query": "quiet title"}),
        }]);

        let first = provider.complete(a_request()).await.unwrap();
        assert_eq!(first.tool_calls.len(), 1);
        assert_eq!(first.tool_calls[0].name, "search_case_law");

        // The round is spent: the next completion is a plain reply, which
        // is the session loop's "call executed, re-prompt" step.
        let second = provider.complete(a_request()).await.unwrap();
        assert!(second.tool_calls.is_empty());
        assert_eq!(second.messages[0].content, "Stub reply 2.");
    }

    #[tokio::test]
    async fn an_injected_failure_fails_the_completion_and_never_reports_usage() {
        let mut provider = StubModelProvider::new();
        provider.set_usage(1_000, 500);
        provider.set_failure(Some("vertex upstream 500".into()));

        let error = provider
            .complete(a_request())
            .await
            .expect_err("an injected failure fails the completion");
        assert!(error.to_string().contains("vertex upstream 500"));

        // The failed call was attempted (recorded), but an `Err` carries
        // no `CompletionResult` — no usage exists to price, so a metering
        // caller debits nothing for it. Nothing here can assert a number
        // of debited cents because the failure produced no usage at all.
        assert_eq!(provider.calls().len(), 1);
    }

    #[test]
    fn the_stub_prices_its_fixed_usage_at_its_rates() {
        let mut provider = StubModelProvider::new();
        provider.set_usage(1_000_000, 500_000);
        provider.set_rates(125, 0);
        let usage = crate::CompletionResult {
            messages: vec![],
            input_tokens: 1_000_000,
            output_tokens: 500_000,
        };
        // 1M input tokens at 125 ¢/MTok = 125¢; the output leg is free at
        // rate 0. The usage ledger's 125¢ debit in the spec's example
        // comes exactly from configuring a stub like this.
        assert_eq!(provider.price_cents(&usage), 125);
    }
}
