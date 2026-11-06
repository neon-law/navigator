---
title: Contingency Term
description: >-
  A matter's percentage-of-recovery fee, as dated insert-only rows that reference the signed fee-agreement Notation.
---

A **Contingency Term** is the [Fee Structure](fee-structure.md) for a matter whose fee is a share of what the client
recovers. Rows are dated by `effective_on` and only ever added to, never edited or deleted, so the table is its own
history. Resolution takes the latest row effective on or before the date asked, the same rule an [Attorney
Rate](attorney-rate.md) follows.

Each row holds:

- **Tiers**: an ordered list of ([Case Stage](case-stage.md), fee in basis points). A tier applies from its stage onward
  in the [Firm](firm.md)'s stage order, so a matter that resolves at trial pays the trial tier and one that settles
  earlier pays the tier for the stage it settled in. A resolution before the first tier's stage pays the first tier.
- **Costs deducted**: `before_fee` takes the advanced [Project Costs](project-cost.md) off the gross recovery before the
  percentage applies; `after_fee` applies the percentage to the gross recovery.
- An optional hybrid hourly `rate_cents_per_hour`, for an agreement that also bills time.
- The signed fee-agreement [Notation](notation.md) on the same matter. The generic retainer carries no amount; the
  contingent fee lives in the separate signed writing the engagement letter requires, and the term points at it.
- The firm-side [Person](person.md) who recorded it.

Amounts are integer cents and percentages are basis points (`3330` is 33.3%), never floats. Only a firm-side participant
on the matter may record or read a term. It never reaches a client or clerk [Lens](lens.md).

- Vocabulary: [`store::contingency_terms`](../../store/src/contingency_terms.rs)

The table is `contingency_term`.
