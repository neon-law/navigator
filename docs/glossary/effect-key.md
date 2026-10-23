---
title: "Effect Key"
description: "An Effect Key is the deterministic name of a durable step's side effect, so a repeated step writes once."
---

The deterministic name of one durable [Step](step.md)'s side effect: `{workflow_id}/{step_id}`, returned by
[`workflows::dbos_engine::effect_key`](../../workflows/src/dbos_engine.rs). DBOS commits a step's checkpoint after its
body returns, so a crash between the two re-runs the body. The re-run carries the same Effect Key, so a PostgreSQL write
stores it in a unique column in the same transaction, and a provider call passes it as an idempotency key. The rule is
in [`docs/dbos-durability.md`](../dbos-durability.md); [Durable execution](durable-execution.md) is the property it
protects.
