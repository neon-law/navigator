---
title: "Outstanding Balance"
description: >-
  The Outstanding Balance is what a matter's client still owes: invoiced minus paid across the matter's Matter Invoices,
  drafts and voided invoices excluded.
---

**Outstanding Balance** is one of the two per-matter financial projections, beside [Work In
Progress](work-in-progress.md). It is what the client still owes on a [Project](project.md), in integer cents, never
floats: the sum of `amount_cents − amount_paid_cents` over every [Matter Invoice](matter-invoice.md) on the matter, with
drafts left out — not yet billed — and voided invoices left out — an invoice nobody owes anything for. An overpaid
invoice contributes its negative remainder rather than being clamped, so the sum stays honest about the matter's true
position.

The balance reads the invoice ledger, never Stripe live, and it is derived in `store::matter_financials` for the matter
dashboard's summary strip and Billing surface.

- Vocabulary: [`store::matter_financials`](../../store/src/matter_financials.rs)
