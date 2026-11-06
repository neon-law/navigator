---
title: Fee Structure
description: >-
  How an attorney's hour or a matter is priced — effective-dated hourly rates, a matter's current flat fee, or a
  contingent share of the recovery.
---

A **Fee Structure** is how the firm prices work: an attorney's hourly rate, a matter's flat fee, or a [Contingency
Term](contingency-term.md). An [Attorney Rate](attorney-rate.md) prices one [attorney's](person.md) hour either
firm-wide or as a per-matter override; rate rows are effective-dated and never edited, so the table is its own history,
and [Time Entries](time-entry.md) carry the rate they were captured under, immune to every later edit. A matter's flat
fee is the currently agreed figure, not history. A Contingency Term prices a matter as a share of its recovery, with
dated, insert-only rows that reference the signed fee-agreement [Notation](notation.md). Amounts are integer cents and
percentages are basis points, never floats.

- Vocabulary: [`store::attorney_rates`](../../store/src/attorney_rates.rs)
