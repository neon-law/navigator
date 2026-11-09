---
title: "Plan Intake"
description: >-
  The prepare-and-confirm flow that stages a parsed plan-assignment notice for an attorney's attested acceptance before
  any matter exists.
---

**Plan Intake** is the prepare-and-confirm flow for a [Legal Plan](legal-plan.md) assignment notice: the parsed notice
is staged as a [Staged Assignment](staged-assignment.md) — member anchored, conflicts checked, plan columns and fee
quote prepared — and then stops. No matter exists until an [attorney](person.md) attests on the review screen.
Acceptance is the only step that opens the [Project](project.md): the plan columns land on it, the acceptance deadline
(measured from the moment the assignment email landed) and turnaround deadline are recorded from the plan's service
levels, the plan's reply and the member's intro message go out, and the accepted fee quote is kept on the matter for the
margin queries that read the [Plan Fee Schedule](plan-fee-schedule.md). A staged assignment may instead be declined with
a reason; either way the review row keeps who decided and when.

- Vocabulary: [`store::plan_intake`](../../store/src/plan_intake.rs)

The review record is `plan_intake`, authored in SurrealDB as pre-matter intake state; the opened [Project](project.md)
carries the durable outcome.
