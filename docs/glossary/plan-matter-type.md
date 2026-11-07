---
title: Plan Matter Type
description: >-
  The closed vocabulary of legal-plan matter kinds shared by fee schedules, plan matters, and the assumption keys.
---

A **Plan Matter Type** is the kind of work a [Legal Plan](legal-plan.md) assignment is: `document_review`, `letter`,
`uncontested_divorce`, or `court_appearance`. The vocabulary is closed — a value outside it never enters the database —
and one value is shared by three surfaces: a [Plan Fee Schedule](plan-fee-schedule.md) row prices one, a plan matter's
[Project](project.md) carries it as `plan_matter_type`, and the Firm's assumption keys name it as their last path
segment. Sharing one vocabulary is what makes "the same type of matter" a concrete definition the reporting can measure
against.

- Vocabulary: [`store::legal_plans`](../../store/src/legal_plans.rs)
