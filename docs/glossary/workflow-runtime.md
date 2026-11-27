---
title: "Workflow Runtime"
description: "The Workflow Runtime is the trait abstraction over Navigator’s durable executor."
---

The [`workflows::StateMachineRuntime`](../../workflows/src/runtime.rs) trait defines how a caller starts a Notation and
signals a transition. The application uses [`DbosNotationRuntime`](../../workflows/src/runtime_dbos.rs) to submit
authenticated requests to the DBOS executor. Tests can use `InMemoryRuntime`.

The [Notation transport](../../workflows/src/notation_http.rs) handles request serialization, bearer authentication,
trace propagation, and bodyless reads. Approval uses dedicated handlers that bind a decision to its review revision.

Each transition records a [Notation Event](notation-event.md). DBOS uses PostgreSQL checkpoints to resume work at the
correct state after a process restarts.
