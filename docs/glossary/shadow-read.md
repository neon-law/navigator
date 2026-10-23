---
title: "Shadow Read"
description: >-
  A Shadow Read runs the PostgreSQL query that will replace a SurrealDB read beside it and records whether both agree.
---

SurrealDB still answers the caller. For a sampled share of a named store's reads, the equivalent PostgreSQL query runs
in a detached, time-bounded task, and both rows are compared through the mirror's own normalization. Each verdict
(match, mismatch, or unavailable) is counted and logged by table and row UUID only, never by column value. A store's
reads cut over only after its Shadow Reads meet the thresholds in
[`docs/postgres-tooling.md`](../postgres-tooling.md#shadow-reads). The settings and comparisons live in
[`store::postgres_shadow`](../../store/src/postgres_shadow.rs).
