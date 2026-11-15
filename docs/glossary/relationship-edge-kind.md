---
title: "Relationship Edge Kind"
description: >-
  A Relationship Edge Kind is one value of the closed vocabulary that names what a Relationship Edge asserts.
---

The `kind` a [Relationship Edge](relationship-edge.md) carries, which names the tie it asserts. Every kind reads "A
`<kind>` B": the edge is directed, from A to B, and has a [Person](person.md) or an [Entity](entity.md) on each end
unless its entry narrows the endpoints. In the Apache AGE `navigator_graph` projection the kind is the `kind` property
of a `RELATIONSHIP` edge.

The vocabulary is closed. A new kind is added by a glossary change first — an entry here and a line in this list — and
only then written by code. More kinds are expected later; until a kind is in this list, the tie is recorded with the
nearest kind below or as a [Relationship Log](relationship-log.md) observation.

- [`adverse_to`](adverse-to-edge.md) — A's interests are opposed to B's in a matter or dispute.
- [`represents`](represents-edge.md) — A acts as counsel for B.
- [`retains`](retains-edge.md) — A engages B to act for it: counsel, a collector, or a vendor of services.
- [`agent_of`](agent-of-edge.md) — A acts on B's behalf with authority: a registered agent, a collector, or a
  signatory.
- [`employs`](employs-edge.md) — A, an Entity, employs B, a Person.
- [`vendor_of`](vendor-of-edge.md) — A supplies goods or services to B.
- [`co_counsel_with`](co-counsel-with-edge.md) — A and B jointly represent the same party.
- [`of_counsel_to`](of-counsel-to-edge.md) — A is of counsel to B, a law firm.
- [`referred`](referred-edge.md) — A referred B to the Firm; A is a referral source.
- [`reports_to`](reports-to-edge.md) — A reports to B within an organization.
- [`related_party`](related-party-edge.md) — A and B are related: family, affiliates, or common control.

Each entry names the spellings it retires. A [Project Reference](project-reference.md) `reason` maps to one of these
kinds; that entry lists the mapping.

Governance and ownership are not Relationship Edge Kinds. They are [Person–Entity Roles](personentity-role.md), the
`entity_role` tie (`HAS_ROLE` in the graph), spelled as single nouns: `manager`, `owner`, `member`, `officer`,
`trustee`, and `beneficiary`.

The store accepts `kind` as open text today and writes only `adverse_to` and `related_party` (see
[`store::relationships`](../../store/src/relationships.rs)). Refusing a kind outside this list is the target design.
