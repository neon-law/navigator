---
title: Outcome Estimate
description: >-
  A lawyer's dated view of how a contingent matter ends: odds and gross recovery for winning at trial, settling, and
  losing.
---

An **Outcome Estimate** is a lawyer's current judgment of how a matter will end. It names exactly three outcomes, each
with a probability in basis points and a gross recovery in integer cents:

| Outcome | Meaning |
| --- | --- |
| `win_at_trial` | The matter is tried and won. |
| `settle` | The matter settles before trial. |
| `lose` | The client recovers nothing; its recovery is always 0. |

The three probabilities sum to 10000. Each estimate also records its author, when it was recorded, the expected
resolution date, and an optional note. Rows are only ever added to, like a [Case Stage](case-stage.md)'s history: the
newest row is the current estimate, and the older rows are the record of how the lawyer's view moved.

The lawyer sets the odds by hand. When a matter with a [Contingency Term](contingency-term.md) has no estimate, its
current stage's Firm default odds stand in, always labeled a default, so nobody reads a Firm default as a lawyer's
judgment. A default carries odds but no recovery. Once a [Matter Outcome](matter-outcome.md) is recorded, the actual
figure replaces the estimate.

An estimate is firm work product. Only a firm-side participant may record or read one. It never loads on a client or
clerk [Lens](lens.md), and never on a [Litigation Pitch](litigation-pitch.md).

- Vocabulary: [`store::outcome_estimates`](../../store/src/outcome_estimates.rs)
