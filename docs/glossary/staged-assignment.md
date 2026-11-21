---
title: "Staged Assignment"
description: >-
  One parsed Legal Plan notice held for lawyer review, with the referral identifiers and any resolved member and fees.
---

A **Staged Assignment** holds a parsed [Legal Plan](legal-plan.md) notice for [Plan Intake](plan-intake.md). The review
shows the provider's [Plan Case Number](plan-case-number.md), member ID, and any certificate number separately. It shows
the requested work as the provider describes it, and any suggested [Plan Matter Type](plan-matter-type.md) and fee
quote. The lawyer can verify the Plan Case Number against the original notice when the staged row has none.

The row links a Client [Person](person.md) and Human [Entity](entity.md) when the member's email is known or the lawyer
confirms it. It carries the [conflict check](conflict-check-graph.md) result for the lawyer's review before any Project
opens. Staging the same inbound Message returns the same review row. Taking the referral opens a [Pitch](pitch.md) and
records the decision. Declining records a reason on the row.

- Vocabulary: [`store::plan_intake`](../../store/src/plan_intake.rs)

PostgreSQL holds the queue in `plan_intake`.
