---
title: "Plan Intake"
description: >-
  Lawyer review that confirms a Legal Plan member, records conflict attestation, and opens a Pitch.
---

**Plan Intake** stages a parsed [Legal Plan](legal-plan.md) notice as a [Staged Assignment](staged-assignment.md). The
lawyer confirms any missing member details, reviews the conflict result, and confirms the [Project](project.md) code.
Taking the referral records the lawyer's attestation and opens a [Pitch](pitch.md) with the Firm's Legal Plan, the [Plan
Case Number](plan-case-number.md), and any member ID. The workflow records internal [Deadlines](deadline.md) from the
plan's service levels. A [Plan Fee Schedule](plan-fee-schedule.md) supplies a fee quote when the jurisdiction and
confirmed [Plan Matter Type](plan-matter-type.md) match a schedule.

Taking the referral sends no email. The lawyer reviews correspondence and handles provider confirmation separately. The
lawyer can also decline with a reason. The review row records who decided and when.

- Vocabulary: [`store::plan_intake`](../../store/src/plan_intake.rs)
