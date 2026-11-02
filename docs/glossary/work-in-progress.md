---
title: "Work In Progress"
description: >-
  Work In Progress is the unbilled value of a matter: billable time at its snapshotted rates plus confirmed costs no
  invoice has drawn.
---

**Work In Progress** (WIP) is one of the two per-matter financial projections, beside the [Outstanding
Balance](outstanding-balance.md). It is the value of work performed on a [Project](project.md) that no [Xero
Invoice](xero-invoice.md) has yet drawn, in integer cents, never floats: unbilled billable [Time Entries](time-entry.md)
priced at each entry's snapshotted rate (`rate_cents_per_hour`, frozen at creation so a later [Fee
Structure](fee-structure.md) edit never reprices captured work) plus [Project Costs](project-cost.md) whose amount is
confirmed and whose billing status is still unbilled. An estimated cost is not yet the firm's money and stays out of the
projection; a written-off entry is a deliberate non-collection and stays out too.

The projection is read-only — it writes nothing to Xero, whose books remain the books — and it is derived in
`store::matter_financials` for the matter dashboard's summary strip and Billing surface.

- Vocabulary: [`store::matter_financials`](../../store/src/matter_financials.rs)
