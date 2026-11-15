---
title: "Co-Counsel With Edge"
description: >-
  A Co-Counsel With Edge records that two lawyers or law firms jointly represent the same party.
---

The Relationship Edge kind `co_counsel_with`: A and B jointly represent the same party. Each also carries a [Represents
Edge](represents-edge.md) to that party.

- Kind: `co_counsel_with`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A to B. The tie is symmetric in meaning, so it is recorded once.
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end; both are lawyers or law firms.
- Example: Two law firms are co_counsel_with each other for one plaintiff.
- Retired aliases: `co_counsel` (a firm-view tie in `graph_firm_view`) maps to `co_counsel_with`.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
