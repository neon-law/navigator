# gateway

The [model gateway](../docs/glossary/model-gateway.md): one `ModelProvider` contract — a completion plus its price in
integer cents — that every Legal Sandbox model backend answers.

## Modules

- `lib.rs` — `ModelProvider`, `CompletionRequest`/`CompletionResult`,
  `tokens_to_cents`, and re-exports of the sandbox skill catalog (`SkillId`, `render_system_context`).
- `vertex.rs` — `VertexProvider`: Claude Sonnet on GCP Vertex AI's
  partner-model API (`:rawPredict`), Application Default Credentials in production.
- `stub.rs` — `StubModelProvider`, the `billing` crate's
  deterministic-stub pattern for dev and tests: fixed token counts, an injectable failure mode, recorded calls.

## Rules

- **Costs are integer cents, never floats.** `price_cents` feeds the usage
  meter directly; token rates are cents per million tokens, summed in `u128` and rounded up to the whole cent.
- **A failed completion costs nothing.** `complete` returns `Err` and
  reports no usage; the meter only ever sees successful exchanges.
- **No identity in the payload.** A request carries the sanitized
  conversation, the session's skills, and the tool catalog — never a person id. Sanitization is the session layer's job.

## Testing

`cargo test -p gateway` — the Vertex provider is exercised against `wiremock`'s in-process HTTP server; the stub and
pricing are pure.
