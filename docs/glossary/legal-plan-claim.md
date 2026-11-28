---
title: Legal Plan Claim
description: >-
  Completed work and its coverage, submission, decision, and payment evidence on a private Project.
---

A **Legal Plan Claim** records work on a private [Project](project.md) that may be payable by a [Legal
Plan](legal-plan.md). The [Plan Case Number](plan-case-number.md) identifies the referral. Each claim has its own id,
benefit, service date, attorney time, and verified fee terms, and obtains a provider reference after submission.
Multiple claims can share one case number. A correction stays on the same claim. A claim filed by email is submitted by
the sent message's id and takes its provider reference when the plan answers. Its [Claim Packet](claim-packet.md) is
filed on the Project and linked to it.

Evidence records the authenticated actor, event date, and structured facts about the work, submission, decision, or cash
received. An estimate, claimed amount, approved outstanding amount, and confirmed net cash are separate amounts. The
claim stays on the Project through closure. [Legal plans](../legal-plans.md#claims-and-collections) explains the
operator workflow.

The table is `legal_plan_claim`.
