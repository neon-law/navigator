---
title: Matter Outcome
description: >-
  What actually happened when a matter resolved, and the fee and costs the firm received.
---

A **Matter Outcome** records how a matter resolved: its result (`won`, `settled`, `lost` or `dismissed`), the date, the
gross recovery, the fee received, and the costs recovered, all in integer cents. It may name the [IOLTA
Allocation](iolta-allocation.md) the recovery passed through, and it names the firm-side [Person](person.md) who
recorded it.

A matter has one Matter Outcome. A correction is a new row that supersedes the current one, never an edit, so the chain
of rows is the audit trail and its newest link is the outcome. Calibration compares these actual figures against the
matter's [Outcome Estimates](outcome-estimate.md). Once a Matter Outcome exists, its actual figure supersedes any
estimate.

A Matter Outcome is firm work product. Only a firm-side participant may record or read one, and it never reaches a
client or clerk [Lens](lens.md).

- Vocabulary: [`store::matter_outcomes`](../../store/src/matter_outcomes.rs)

The table is `matter_outcome`.
