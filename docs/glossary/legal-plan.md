---
title: Legal Plan
description: >-
  A panel program a Firm performs work for, held as Firm-scoped rows with the plan's own contact and service levels.
---

A **Legal Plan** is a panel program the [Firm](firm.md) performs work for — ARAG, LegalShield, and the like. The
vocabulary is [Firm](firm.md) data: two firms on one deployment each hold their own row for the same program, with their
own contact and service levels, the way a [Case Stage](case-stage.md) vocabulary is Firm data rather than a shared
dictionary. The Firm responds within one business day by default. A plan can specify a response window in hours or a
completion term in days. An accepted matter has a completion deadline only when a term is recorded.

What a plan pays for one covered benefit lives in a [Plan Fee Schedule](plan-fee-schedule.md), which names its plan. A
plan matter is a [Project](project.md) linked to its plan row, carrying the plan's [Plan Case
Number](plan-case-number.md). The [Legal plans](../legal-plans.md) guide lists where the Firm logs into each plan and
how a referral becomes a paid matter.

- Vocabulary: [`store::legal_plans`](../../store/src/legal_plans.rs)

The table is `legal_plan`.
