---
title: "Workflow Runtime"
description: "The Workflow Runtime is the trait abstraction over Navigator’s durable executor."
---

The trait abstraction over the durable executor — [`workflows::StateMachineRuntime`](../../workflows/src/runtime.rs).
Application adapters include:

- **`InMemoryRuntime`** — non-durable, in-process. Used by tests and by `cargo run -p neon` when DBOS Notation
  is disabled and no Restate broker is configured. Reset on each process start.
- **`RestateRuntime`** — HTTP adapter that talks to a [Restate](restate.md) broker
  ([`workflows/src/runtime_restate.rs`](../../workflows/src/runtime_restate.rs)). Explicit compatibility selection
  retains its broker environment contract.
- **`DbosNotationRuntime`** — HTTP adapter that talks to the worker's DBOS ingress
  ([`workflows/src/runtime_dbos.rs`](../../workflows/src/runtime_dbos.rs)). Selected only when
  `NAVIGATOR_DBOS_NOTATION_ENABLED=true`, which takes precedence over the Restate broker and fails the boot when the
  worker's ingress is not configured.

A Workflow Runtime is started once per Notation (`start(notation_id, spec)`) and advanced by external
`signal(notation_id, spec, condition)` calls. Every transition is recorded as a [Notation Event](notation-event.md) so a
crash plus replay terminates in the same state.

The HTTP adapters share the internal [Notation transport](../../workflows/src/notation_http.rs). It owns request
serialization, bearer authentication, trace propagation, bodyless reads, and transport errors. Native DBOS does not
construct the legacy broker adapter. DBOS approval uses its revision-bound approval and progress handlers; explicit
legacy callers retain their signal-based approval behavior.
