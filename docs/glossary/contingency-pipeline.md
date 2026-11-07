---
title: "Contingency Pipeline"
description: >-
  The lawyer workbench card summing projected cash across the caller's own open contingent matters.
---

The **Contingency Pipeline** is a card on the lawyer workbench (`/app/lawyer`). It sums the [Outcome
Projection](outcome-projection.md) of each open contingent matter the caller can see, and shows:

- the expected fees and expected profit in total, with the range of profit the outcomes span;
- the costs at risk, which are the costs advanced that a loss would not recover;
- the expected cash (fee plus costs recovered) by quarter of each [Outcome Estimate](outcome-estimate.md)'s expected
  resolution date, as an inline SVG chart; an overdue date counts in the current quarter;
- the matters ranked by expected value, with matters whose odds carry no recovery (a stage default) last;
- the matters whose estimate is stale.

It reads the same `store::access::visible_projects_as_lawyer` collection as the workbench KPIs, and Owner and Admin get
no bypass, so it never counts a matter the KPI list would hide. A resolved matter (one with a [Matter
Outcome](matter-outcome.md)) is realized cash, not pipeline, and stays out. Firm work product.

- Vocabulary: [`store::projected_cash`](../../store/src/projected_cash.rs) · Component:
  [`webapp::projected_cash`](../../webapp/src/projected_cash.rs)
