---
title: "Related Party Edge"
description: >-
  A Related Party Edge records that two parties are related by family, affiliation, or common control.
---

The Relationship Edge kind `related_party`: A and B are related — family, affiliates, or entities under common control.
It is a softer signal than an [Adverse To Edge](adverse-to-edge.md): the conflict check flags it for lawyer review
rather than blocking.

- Kind: `related_party`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A to B. The tie is symmetric in meaning, so it is recorded once.
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: Two companies under common control are related_party to each other.
- Retired aliases: The duplicate `related_party` definitions in
  [`store::relationships`](../../store/src/relationships.rs) (`KIND_RELATED_PARTY`) and
  [`store::disclosures`](../../store/src/disclosures.rs) (a [Disclosure](disclosure.md) kind) name one tie; this entry
  is its single definition.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
