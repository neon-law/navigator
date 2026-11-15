---
title: "Represents Edge"
description: >-
  A Represents Edge records that one party acts as counsel for another.
---

The Relationship Edge kind `represents`: A acts as counsel for B. It records opposing counsel, the client's counsel in
another matter, and co-counsel's client, so a later matter can collide with the lawyer or law firm as well as with the
party.

- Kind: `represents`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (counsel) to B (the represented party).
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end; A is a lawyer or a law firm.
- Example: A law firm represents a plaintiff.
- Retired aliases: None stored. Opposing counsel and co-counsel were described only in prose.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
