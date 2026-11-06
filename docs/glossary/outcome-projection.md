---
title: "Outcome Projection"
description: >-
  The outcome projection matter section: a contingent matter's projected profit by outcome, on the lawyer lens only.
---

The `outcome_projection` [Section](section.md) on a [Matter Page](matter-page.md): a contingent matter's projected
profit. It shows one bar per outcome (win at trial, settle, lose) with that outcome's net and odds, the expected value,
the costs at risk, where the odds came from, the date of the [Outcome Estimate](outcome-estimate.md), and a stale flag
when the newest [Case Stage](case-stage.md) move is later than that estimate.

The odds come from the lawyer's current estimate, from the current stage's Firm default when there is no estimate, or
from the [Matter Outcome](matter-outcome.md) once the matter resolves, and the section always says which. Each net is
the [Contingency Term](contingency-term.md) fee plus costs recovered, less the costs advanced and the logged time at its
snapshotted rate. A matter with no Contingency Term shows the empty state.

Firm work product: `rules::dashboard::FIRM_WORK_PRODUCT` keeps it on the lawyer [Lens](lens.md) of every kind, so `D005`
refuses it on a client or clerk lens, and no [Litigation Pitch](litigation-pitch.md) catalog carries it. Section name
`outcome_projection`, optional on the Matter Status Console, Docket Deadline Board and Engagement and Billing Records
kinds.

- Vocabulary: [`store::matter_projections`](../../store/src/matter_projections.rs) · Component:
  [`webapp::sections::outcome_projection`](../../webapp/src/sections/outcome_projection.rs)
