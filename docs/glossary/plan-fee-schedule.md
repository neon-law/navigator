---
title: Plan Fee Schedule
description: >-
  What a Legal Plan pays for one covered benefit, held as effective-dated, append-only rows per Firm.
---

A **Plan Fee Schedule** row is what a [Legal Plan](legal-plan.md) pays for one covered benefit — a document review, a
letter, an uncontested divorce, a court appearance — in one state. Rows are dated by `effective_on` and only ever added
to, never edited or deleted, so the table is its own history. Resolution takes the latest row effective on or before the
date asked, the same rule an [Attorney Rate](attorney-rate.md) follows.

Each row holds the [matter type](plan-matter-type.md), the state's jurisdiction code, the benefit it covers, what the
plan pays (`plan_fee_cents`), and the supplemental amounts the Firm may bill beyond the benefit. Amounts are integer
cents, never floats. A row must name a plan of the same [Firm](firm.md) — the database refuses a schedule row that
points across firms, so one firm's pricing never quotes another firm's work. Fee schedules are Firm data: repository
carries neutral sample rows only.

- Vocabulary: [`store::legal_plans`](../../store/src/legal_plans.rs)

The table is `plan_fee_schedule`.
