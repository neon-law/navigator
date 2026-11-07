---
title: "Projected Contingency Cash"
description: >-
  The Firm show page's rollup of expected contingency cash by quarter, brand and lead lawyer, naming no matter.
---

**Projected Contingency Cash** is a section on the Firm show page, beside the trailing-30-day invoice graphs, for Owner
and Admin. It sums the expected cash (fee plus costs recovered) of every open contingent matter the Firm owns, from the
same [Outcome Projection](outcome-projection.md) the [Contingency Pipeline](contingency-pipeline.md) reads, and shows:

- the expected cash in total, with the range the outcomes span;
- expected cash by quarter of expected resolution, beside the USD invoiced and paid on invoices issued that quarter,
  for the three quarters before the current one onward, with anything past seven quarters out in one `Later` bucket;
- totals by brand and by lead lawyer ([DRI](directly-responsible-individual-dri.md)), where a matter with no lead
  lawyer counts under `Unassigned`;
- a concentration warning when one matter holds more than a quarter of the Firm's expected cash;
- a count of open contingent matters whose odds carry no recovery yet.

**Privacy.** `store::projected_cash::firm_rollup` builds every label, the rule the invoice graphs follow: a brand's
display name, a lead lawyer's display name, or `Unassigned`. No Project code, matter name, client name, or invoice id
reaches the section, and the concentration warning states a share, never the matter.

- Vocabulary: [`store::projected_cash`](../../store/src/projected_cash.rs) · Component:
  [`webapp::projected_cash`](../../webapp/src/projected_cash.rs)
