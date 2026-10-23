---
title: "Mirror Backfill"
description: >-
  A Mirror Backfill copies SurrealDB rows the PostgreSQL write mirror never saw, in checkpointed, resumable batches.
---

The write mirror only sees writes made after it was switched on. A Mirror Backfill walks every mirrored group, parents
first, writes each row SurrealDB holds and PostgreSQL lacks through the mirror's own conversion, and checkpoints every
batch in `mirror_backfill_checkpoint`. A run resumes by its id without writing a row twice. A row that still cannot land
is recorded in `mirror_backfill_failure` by table and UUID with a sanitized reason, never by value. Rows that differ, or
that only PostgreSQL holds, are left to the mirror's reconciliation and keep the run's parity report unclean. Each
invocation reports per-table drift counts and up to 50 divergent UUID keys; other key forms are counted only. Run
`navigator ops postgres backfill`; see [`docs/postgres-tooling.md`](../postgres-tooling.md#mirror-backfill) and
[`store::postgres_backfill`](../../store/src/postgres_backfill.rs).
