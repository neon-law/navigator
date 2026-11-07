---
title: Legal Plan
description: >-
  A panel program a Firm performs work for, held as Firm-scoped rows with the plan's own contact and service levels.
---

A **Legal Plan** is a panel program the [Firm](firm.md) performs work for — ARAG, LegalShield, and the like. The
vocabulary is [Firm](firm.md) data: two firms on one deployment each hold their own row for the same program, with their
own contact and service levels, the way a [Case Stage](case-stage.md) vocabulary is Firm data rather than a shared
dictionary. Each row carries the acceptance service level (hours from the assignment email landing) and the turnaround
service level (days from acceptance) that reporting compares actuals against, plus what the plan asks the Firm to
report.

What a plan pays for one covered benefit lives in a [Plan Fee Schedule](plan-fee-schedule.md), which names its plan.

- Vocabulary: [`store::legal_plans`](../../store/src/legal_plans.rs)

The table is `legal_plan`.
