---
title: "Durable execution"
description: "Durable execution is the property the Workflow Runtime gives the application."
---

The property the [Workflow Runtime](workflow-runtime.md) gives the application. Once a Notation has emitted a signal
(say, `retainer_rendered`), the transition is recorded somewhere that survives process restarts; replay reaches the same
terminal state even if the worker crashes mid-flight. [Restate](restate.md) provides this property in production; for the
workflows moved behind `NAVIGATOR_DBOS_*_ENABLED` flags, DBOS Transact provides it on PostgreSQL. `InMemoryRuntime` is
a non-durable simulation for tests and local dev.
