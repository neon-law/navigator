---
title: "Retains Edge"
description: >-
  A Retains Edge records that one party engages another to act for it, such as counsel, a collector, or a vendor.
---

The Relationship Edge kind `retains`: A engages B to act for it — as counsel, as a collector, or as a vendor of
services. It records the engagement itself; when B acts on A's behalf with authority, an [Agent Of
Edge](agent-of-edge.md) records that authority, from B to A.

- Kind: `retains`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the engaging party) to B (the engaged party).
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: A creditor retains a collection agency; a company retains a law firm.
- Retired aliases: `client` (a firm-view tie in `graph_firm_view`) maps to `retains`, read from the client's side.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
