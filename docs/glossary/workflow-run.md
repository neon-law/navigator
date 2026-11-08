---
title: "Workflow Run"
description: "A Workflow Run is one execution of a Workflow in its runtime."
---

A Workflow Run is one execution of a [Workflow](workflow.md). A durable runtime identifies its executions and retains
their progress for recovery. DBOS records execution status in
[`dbos.workflow_status`](../dbos-durability.md#several-replicas), keyed by workflow UUID. A [Workflow
Request](workflow-request.md) can bind that identifier to a request fingerprint.

A [Notation](notation.md) has a workflow machine keyed by its Notation identifier and machine kind. [Notation Machine
State](notation-machine-state.md) records the specification and current state of that machine. The runtime may execute
several operations over the machine's lifetime. A [Notation Transition](notation-transition.md) records one logical
operation across recovery or repair, and a [Notation Event](notation-event.md) records a state change. Navigator has no
separate application table for Workflow Runs.
