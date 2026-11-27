---
title: "Workflow Runtime"
description: "The Workflow Runtime is the trait abstraction over Navigator’s durable executor."
---

[StateMachineRuntime](../../workflows/src/runtime.rs) starts one machine per Notation and advances it when a caller
submits a signal. Each transition records a [Notation Event](notation-event.md).

[DbosNotationRuntime](../../workflows/src/runtime_dbos.rs) submits authenticated requests to the DBOS listener in the
web deployment. The [Notation transport](../../workflows/src/notation_http.rs) handles request serialization, bearer
authentication, trace propagation and transport errors. Revision-bound approvals use dedicated handlers.
`InMemoryRuntime` provides an in-process implementation for tests.
