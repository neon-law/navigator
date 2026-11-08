---
title: Plan Case Number
description: >-
  The number a Legal Plan gives one referral, which every plan email and claim cites, held on the Project.
---

A **Plan Case Number** is the number a [Legal Plan](legal-plan.md) gives one referral, kept exactly as the plan prints
it. ARAG calls it a CaseAssist number (`NNNNNNN-NNN`), and LegalEASE calls it a member case or Case ID. The plan's case
requests, confirmations, acceptance notices, and claims all cite it. That makes it the key that links a plan matter back
to how the matter is paid, and the identifier the [Inbox](inbox.md) routes plan email by.

It is one token of up to 64 visible characters and lives on the [Project](project.md) beside the other plan columns.
`navigator project legal-plan set --case-number` records it. A Plan Case Number is not the plan's member identifier,
which names the person and spans every referral they make.

- Accessor: [`store::projects::set_plan_columns`](../../store/src/projects.rs)
- Guide: [Legal plans](../legal-plans.md)
