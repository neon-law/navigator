---
title: "Adverse To Edge"
description: >-
  An Adverse To Edge records that one party's interests are opposed to another's in a matter or dispute.
---

The Relationship Edge kind `adverse_to`: A's interests are opposed to B's in a matter or dispute. It is the strongest
conflict signal: a confident, direct `adverse_to` link to a current client blocks a new matter in the [Conflict-Check
Graph](conflict-check-graph.md). `navigator project adverse add` writes one from the Project's client to a party the
Project has referenced.

- Kind: `adverse_to`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A to B. Adversity is often mutual, but the edge records the side that asserted it: the client is A.
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: A client is adverse_to a creditor.
- Retired aliases: `adverse_party` (a [Project Reference](project-reference.md) reason), `adversary` (a firm-view tie in
  `graph_firm_view`), and `Reason::Adverse` (the conflict finding in [`store::conflicts`](../../store/src/conflicts.rs))
  name this tie. Each maps to `adverse_to`.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
