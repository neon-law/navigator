---
title: "Of Counsel To Edge"
description: >-
  An Of Counsel To Edge records that a lawyer or firm is of counsel to a law firm.
---

The Relationship Edge kind `of_counsel_to`: A is of counsel to B, a law firm — associated with it without being a
partner or an associate.

- Kind: `of_counsel_to`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the lawyer or firm of counsel) to B (the law firm).
- Endpoints: A is a [Person](person.md) or [Entity](entity.md); B is an [Entity](entity.md), a law firm.
- Example: A retired partner is of_counsel_to a law firm.
- Retired aliases: `of_counsel` (a firm-view tie in `graph_firm_view`) maps to `of_counsel_to`.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
