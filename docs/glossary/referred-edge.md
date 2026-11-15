---
title: "Referred Edge"
description: >-
  A Referred Edge records that one party referred another to the Firm as a referral source.
---

The Relationship Edge kind `referred`: A referred B to the Firm, so A is a referral source. It runs the opposite way
from a [Referral](referral.md), which is a Matter the Firm hands to outside counsel.

- Kind: `referred`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the referral source) to B (the referred prospect or client).
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: A former client referred a prospect.
- Retired aliases: None; the kind is new.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
