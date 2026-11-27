---
title: "Durable execution"
description: "Durable execution is the property the Workflow Runtime gives the application."
---

The property the [Workflow Runtime](workflow-runtime.md) gives the application. Once a Notation has emitted a signal
(say, `retainer_rendered`), the transition is recorded somewhere that survives process restarts; replay reaches the same
terminal state even if the worker crashes mid-flight. DBOS Transact journals execution in PostgreSQL. Tests can exercise
the state machine with `InMemoryRuntime`.
