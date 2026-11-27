---
title: "Mirror Backfill"
description: >-
  A Mirror Backfill copies SurrealDB rows the PostgreSQL write mirror never saw, in checkpointed, resumable batches.
---

The write mirror only sees writes made after it was switched on. A global Mirror Backfill walks every mirrored group,
parents first, writes each row SurrealDB holds and PostgreSQL lacks through the mirror's own conversion, and checkpoints
every batch in `mirror_backfill_checkpoint`. A run resumes by its id without writing a row twice. A row that still
cannot land is recorded in `mirror_backfill_failure` by table and UUID with a sanitized reason, never by value. Rows
that differ, or that only PostgreSQL holds, are left to the mirror's reconciliation and keep the run's parity report
unclean. Each invocation reports per-table drift counts and up to 50 divergent UUID keys; other key forms are counted
only. The tables born in PostgreSQL have no write mirror; a separate arm of the run copies their earlier SurrealDB rows
the same way. Run `hudson ops postgres backfill`; see
[`docs/postgres-tooling.md`](../storage-models.md#historical-admission) and
[`store::postgres_backfill`](../../store/src/postgres/history.rs).

A run's `scope` is `global` for the existing complete inventory or an immutable registered family version such as
`satellites:1`. A scoped run binds its exact table checkpoints to a verified operational export receipt. Pending scoped
imports may refresh source-owned rows, but never delete target-only rows or adjudicate conflicting PostgreSQL Person
rewrites. Completing every content, reference, and constraint proof activates that family; later imports cannot replay
its history, even when the first global run has not completed.

`source_receipt` binds the strict capture's bytes, digest, schema, and raw table census. A verified replacement is
allowed only while pending; `source_receipt_history` retains each superseded receipt. Completed receipts and their
checkpoints are durable authority evidence, retained through logical and physical recovery rather than discarded as job
history.

The table is `mirror_backfill_run`.
