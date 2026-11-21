---
title: Legal Plan Claim Event
description: >-
  Append-only evidence for a change to a private Project's Legal Plan Claim.
---

A **Legal Plan Claim Event** records a change to a [Legal Plan Claim](legal-plan-claim.md). It stores the authenticated
actor, event date, recording time, and structured evidence. Provider filing and remittance references allow the Firm to
reconcile money and identify duplicate filings. A repeated event key with identical evidence returns the current claim.

The table is `legal_plan_claim_event`.
