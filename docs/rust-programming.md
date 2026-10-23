# Rust programming

Navigator is application code. The public Rust guides come first; advice for published library APIs is advisory.

- [Rust Style Guide](https://doc.rust-lang.org/style-guide/) and the [rustfmt
  reference](https://rust-lang.github.io/rustfmt/)
- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [Clippy lint index](https://rust-lang.github.io/rust-clippy/master/)
- [The Rust Programming Language](https://doc.rust-lang.org/book/), [Rust by
  Example](https://doc.rust-lang.org/rust-by-example/), the [async book](https://rust-lang.github.io/async-book/), and
  the [edition guide](https://doc.rust-lang.org/edition-guide/)
- [Google's Rust Style Guide](https://google.github.io/styleguide/rustguide.html), secondary; see [where we
  differ](#where-we-differ-from-google)

## Workspace defaults

- `rust-toolchain.toml` and the workspace `Cargo.toml` set the toolchain, edition, and lints. Every member
  crate declares `[lints] workspace = true`.
- `unsafe_code = "forbid"`; clippy pedantic runs with `-D warnings`. When a lint can enforce a rule, enable the lint
  instead of adding prose.
- The stack is Tokio, Axum, and SurrealDB, the source of truth for application reads and writes. PostgreSQL holds the
  Firm journal and an opt-in mirror of selected stores, and is replacing SurrealDB (see the
  [`postgres`](../.agents/skills/postgres/SKILL.md) skill and the [PostgreSQL tooling guide](postgres-tooling.md)).
  `workflows-service`, `archives`, and `billing-workflows` use the Restate SDK directly; `workflows` does not.

## Error handling

- Libraries return typed `thiserror` enums. Binaries return `anyhow::Result<T>` with context at the boundary. HTTP
  handlers return their own typed error implementing `IntoResponse` (`ApiError` in `portal/src/api.rs`, `WebhookError`
  in `portal/src/esignature_webhook.rs`). Public signatures never use `Box<dyn Error>`.
- Outside `main` and tests, no `unwrap` or `panic!`; `expect("invariant: …")` only when a reader can verify the
  invariant in one line.
- Destructure with `if let` or `let … else`; never check then unwrap. Parse external JSON into a typed struct instead
  of unwrapping its shape.

## Types and modules

- Newtypes for cross-module ids, enums for meaningful state, `Option<T>` over sentinels, and borrowed inputs when
  ownership is unnecessary.
- A public function returns a named struct, not a tuple, when two fields share a type.
- One concept per file, named for its primary item. Re-export with `mod` plus `pub use`.

## Comments describe the present

- Comments, docs, and tests state the current contract. Git holds the history. Remove old paths, flags, aliases,
  examples, README rows, and tests with the feature they served.
- Keep the rationale for a live invariant or migration, and guards for current behavior such as required redirects,
  `404`s, or removed columns.

## Async and concurrency

- Prefer `async fn` and structured concurrency. A bare `tokio::spawn` needs an owner and a cancellation path. Bound
  channels; reserve `unbounded_channel` for in-process control planes. Never hold a `std::sync::Mutex` guard across
  `.await` (clippy `await_holding_lock`). Use `spawn_blocking` for blocking work and timeouts around external calls.
- A poisoned `std::sync::Mutex` is a bug; `.lock().expect("…")` propagates it. Recover with `PoisonError::into_inner`
  only where every write leaves the data valid, and say why in a comment.

Restate handlers are stricter: no native concurrency for journaled work. See
[`agent-workflows.md`](agent-workflows.md#restate-handler-authoring) and [`durable-workflows.md`](durable-workflows.md).

## Axum

- Extend the existing router with typed extractors and explicit state. Keep authorization beside the existing helpers.
  New `/app/...` routes follow [`access-model.md`](access-model.md) and embedded Rego; hidden lawyer-only routes return
  `404`.
- Body-consuming extractors (`Json`, `Form`, multipart) go last in a handler's arguments; a body is consumed once.
  `5xx` responses log via `tracing` before returning; `4xx` responses do not.

## The store

- A new application table is relational and named from the glossary. It starts as one top-level `store::<table>`
  module, with a Surreal schema update and `SCHEMA_VERSION` bump in the same commit. Keep source and mirror write paths
  explicit. Every write maintains `inserted_at` / `updated_at`. Re-seeding inserts missing rows idempotently and never
  updates live production rows.

## Service lifecycle

- Long-running binaries initialize dependencies before serving, hold the telemetry guard through `main`, and use
  `portal::hosting::shutdown_signal`. Liveness never probes downstreams; readiness performs the required dependency
  round-trips.

## Testing

- Tests ship with the implementation. Unit tests live beside the code; integration tests under `<crate>/tests/`. Use
  `#[tokio::test]`, `assert_cmd`/`predicates` for CLI smoke tests, and snapshots for stable HTML/JSON shapes. Restate
  changes need replay-aware coverage.
- An `assert!` carries a message when its expression does not name the case under test.
- Run focused tests while implementing. Before a PR, run `navigator validate`, or `cargo run -p cli --quiet --
  validate` when `navigator` is not on `PATH`. CI runs the workspace suite.

## Dependencies and assets

- `cargo update` refreshes the lockfile; `cargo upgrade` changes requirements and needs explicit review. Update crates
  and vendored assets separately. Serve web assets same-origin; never use runtime CDNs.

## Where we differ from Google

- `Arc<dyn Trait>` with `#[async_trait]` is the test-fake seam; it needs no justification.
- Async is the default. Google's async ban is Chromium-specific.
- Builders may accept `impl Into<String>`.
- Internal types skip `#[non_exhaustive]`: a new variant should break every match.
- `mod.rs` and `foo.rs` beside `foo/` may hold code.
- Error and panic sections in doc comments are optional (`missing_errors_doc`, `missing_panics_doc`).

## Before committing Rust

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run -p <crate>
```

Report the exact gate you ran. Prefer failure-only output:

- `cargo nextest run`: `.config/nextest.toml` prints failures only. `features` uses its own harness; run it with
  `cargo test -p features`.
- `cargo build -q --message-format short`; use full diagnostics only when their rendered context is needed.
