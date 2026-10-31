---
title: "Case Stage"
description: "Where a matter sits in the Firm's own ordered workflow — Firm-authored vocabulary with an audited per-matter history."
---
Where a [Matter](matter.md) sits in the [Firm](firm.md)'s own workflow — intake, pleading, discovery, trial prep, and so
on. The vocabulary is **Firm data, not a hardcoded enum**: the Firm names its stages and their order, and each stage may
name the `case_type` it belongs to when the workflow differs by kind of matter. A Case Stage is deliberately distinct
from two neighbors it must never be conflated with. [`project.status`](project.md) is the lifecycle — `open`, `closed`,
`archived` — and lives on the Project itself. `case.status` is the litigation posture before one [forum](case.md) —
`open`, `stayed`, `closed`. Neither answers "where are we in the Firm's process", and UI copy never uses a Case Stage
word for either of them. **The current stage is the newest history row.** A matter holds no mutable stage column: it
holds an append-only `stage_event` — the stage it moved from, the stage it moved to, the [Person](person.md) who moved
it, when, and an optional note. The database itself polices the history: a write must name a stage from the matter
Firm's own vocabulary, must continue the chain from the matter's current stage, and can never be edited or deleted.
Moving backward is legal — a matter can return to discovery — because the Firm's list orders the workflow, not the
calendar. Re-staging to the stage a matter already holds records no event. **Storage.** `stage_definition` and
`stage_event` are PostgreSQL-native (`store/src/case_stages.rs`) — net-new tables with no Surreal source. The Project
must already belong to a Firm and be mirrored to PostgreSQL before it can hold a Case Stage. Schema:
`store/postgres/migrations/20261031130000_create_case_stage.sql`.
