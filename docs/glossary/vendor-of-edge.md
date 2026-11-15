---
title: "Vendor Of Edge"
description: >-
  A Vendor Of Edge records that one party supplies goods or services to another.
---

The Relationship Edge kind `vendor_of`: A supplies goods or services to B. A supplier is often also a creditor; when the
two are opposed in a dispute, an [Adverse To Edge](adverse-to-edge.md) records that as well.

- Kind: `vendor_of`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the supplier) to B (the customer).
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: A supplier is vendor_of a dispensary.
- Retired aliases: None; the kind is new.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
