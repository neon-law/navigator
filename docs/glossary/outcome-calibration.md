---
title: "Outcome Calibration"
description: >-
  The Firm show page's comparison of resolved matters' Outcome Estimates with their Matter Outcomes, naming no matter.
---

**Outcome Calibration** is a section on the Firm show page, after [Projected Contingency
Cash](projected-contingency-cash.md), for Owner and Admin. It scores every resolved matter the Firm owns on the odds in
force thirty days before it resolved: the newest [Outcome Estimate](outcome-estimate.md) recorded on or before that day,
against the matter's current [Matter Outcome](matter-outcome.md). A corrected outcome counts only through its current
row; `lost` and `dismissed` both score as losing. A resolved matter with no estimate that old is counted, not scored.

Once at least ten matters are scored it shows:

- predicted against actual win and settle rates, with each matter placed in a 20-point odds band by the probability it
  was given;
- the mean three-outcome Brier score, from 0 (every outcome called with certainty) to 2 (certain and wrong);
- the mean recovery error, expected gross recovery minus the actual one, and its absolute mean;
- the Brier score and recovery error by lead lawyer ([DRI](directly-responsible-individual-dri.md)) and by the [Case
  Stage](case-stage.md) the matter held when the scored estimate was recorded, with `Unassigned` for neither.

All arithmetic is integer basis points and cents; only the displayed text has a decimal point.

**Privacy.** `store::outcome_calibration::firm_calibration` builds every label: a lead lawyer's display name, a Case
Stage's name, or `Unassigned`. No Project code, matter name, client name, or invoice id reaches the section.

- Vocabulary: [`store::outcome_calibration`](../../store/src/outcome_calibration.rs) · Component:
  [`webapp::outcome_calibration`](../../webapp/src/outcome_calibration.rs)
