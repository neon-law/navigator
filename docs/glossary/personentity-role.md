---
title: "Person–Entity Role"
description: >-
  A Person–Entity Role records a Person's place in an Entity, such as manager, member, beneficiary, or trustee.
---

A Person's role within an Entity, such as `manager`, `member`, `beneficiary`, or `trustee`. These are the structural
ties the [Conflict-Check Graph](conflict-check-graph.md) walks at full confidence — the tie *is* the graph edge, `RELATE
person->entity_role->entity`, rather than a row projected into one.

The `role` is one of six single nouns, the governance and ownership vocabulary that sits beside the closed [Relationship
Edge Kind](relationship-edge-kind.md) list. Each reads "Person is the Entity's `<role>`":

- `manager` — manages the Entity. Retires the verb spelling `manages`.
- `owner` — holds an ownership interest in it. Retires the verb spelling `owns`.
- `member` — is a member, as of a limited liability company.
- `officer` — holds an office, such as president or secretary.
- `trustee` — is a trustee, as of a trust.
- `beneficiary` — is a [Beneficiary](beneficiary.md), as of a trust.

Employment, agency, and representation are not roles; they are Relationship Edges such as [`employs`](employs-edge.md)
and [`agent_of`](agent-of-edge.md). The store accepts `role` as open text today, and tests and fixtures still write
`manages`; holding `role` to these six nouns is the target design.

There is no surrogate key: a tie's identity is its two endpoints plus its `role`, which is what the UNIQUE
`entity_role_tie` index says and what makes re-seeding idempotent.

- Schema and queries: [`store::entity_roles`](../../store/src/entity_roles.rs) — Lives in the
  `entity_role` relation

The table is `entity_role`.

Schema: [`entity_role`](../../store/postgres/schema.sql).
