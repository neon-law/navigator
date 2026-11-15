---
title: "Reports To Edge"
description: >-
  A Reports To Edge records that one person reports to another within an organization.
---

The Relationship Edge kind `reports_to`: A reports to B within an organization. Both usually also carry an [Employs
Edge](employs-edge.md) from the same Entity.

- Kind: `reports_to`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the person who reports) to B (the person reported to).
- Endpoints: A [Person](person.md) on each end.
- Example: An associate reports_to a partner.
- Retired aliases: None; the kind is new.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
