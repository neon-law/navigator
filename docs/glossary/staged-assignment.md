---
title: "Staged Assignment"
description: >-
  One parsed plan-assignment notice held for attorney review: the member anchor, conflict result, plan columns, and fee
  quote, with no matter behind it.
---

A **Staged Assignment** is one parsed [plan-assignment](plan-intake.md) notice held for attorney review. The row carries
the parsed fields as the review screen shows them: the [member](person.md) anchor (a Client Person and their Human
[entity](entity.md), found-or-created from the notice's contact), the [conflict check](conflict-check-graph.md) result
attached before any matter write, the plan columns the matter will open with (`plan_member_id`, the notice's verbatim
`plan_number` — the future [Plan Case Number](legal-plan.md) on the project, [matter type](plan-matter-type.md), state),
and the fee quote resolved from the firm's [Plan Fee Schedule](plan-fee-schedule.md) — or the reason it could not
resolve. Staging is idempotent per inbound message: the same notice re-lands on the same row. The row is decided exactly
once — accepted (which opens the matter) or declined with a reason — and never anchors a matter by itself.

- Vocabulary: [`store::plan_intake`](../../store/src/plan_intake.rs)

The record lives in PostgreSQL whenever it is configured, with SurrealDB as the fallback.

The table is `plan_intake`.
