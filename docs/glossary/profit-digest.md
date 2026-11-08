---
title: "Profit Digest"
description: >-
  The daily Slack post of each Firm's Projected Contingency Cash and Outcome Calibration, naming no matter.
---

The **Profit Digest** is a daily post to the firm's internal messages channel that carries the Firm page's [Projected
Contingency Cash](projected-contingency-cash.md) and [Outcome Calibration](outcome-calibration.md) to people who have
not opened the page. For each Firm with an open contingent matter it shows:

- the expected cash, its range, and the change since the previous day's digest;
- the costs advanced that a loss would not recover;
- expected cash for the current quarter and the next three;
- totals by brand and by lead lawyer ([DRI](directly-responsible-individual-dri.md));
- the concentration warning;
- the Brier score, or how many more resolved matters calibration needs;
- a link to the Firm page.

It reads both rollups as the Firm's Admin DRI and only formats them, so it names no Project code, matter, or client. A
day with no open contingent matter posts nothing.

- Workflow: [`workflows::dbos_engine::profit_digest`](../../workflows/src/dbos_engine/profit_digest.rs) · Rollout:
  [DBOS durability](../dbos-durability.md#profitdigest)
