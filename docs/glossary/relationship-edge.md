---
title: "Relationship Edge"
description: "A Relationship Edge is a typed graph connection between a Person or Entity on each end."
---

A typed graph edge with a [Person](person.md) or [Entity](entity.md) on **each** end — the canonical two-sided
relationship the [Conflict-Check Graph](conflict-check-graph.md) traverses. Distinct from the [Relationship
Log](relationship-log.md), which is a one-sided audit trail (one actor, one subject); a Relationship Edge instead
asserts "A is `adverse_to` B" or "A is a `related_party` of B."

Its `kind` is one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary: `adverse_to`,
`represents`, `retains`, `agent_of`, `employs`, `vendor_of`, `co_counsel_with`, `of_counsel_to`, `referred`,
`reports_to`, and `related_party`. Each reads "A `<kind>` B", from A to B. A new kind enters the glossary before any
code writes it. Governance and ownership stay [Person–Entity Roles](personentity-role.md).

Each edge carries provenance (`source_kind` ∈ `manual` / `disclosure` / `relationship_log` / `llm`) and a
`confidence_pct` (0–100). Human-asserted edges are full confidence; edges an LLM parses out of a Relationship Log's
free-form detail land lower, and the conflict check multiplies confidence along a path so a chain of weak guesses cannot
raise a finding on its own.

Both endpoints identify a Person or Entity. The store validates endpoint kinds before writing the edge.

A matter asserts adversity through `POST /app/api/projects/{id}/relationships/adverse` (`navigator project adverse
add`): a `manual` `adverse_to` edge at `confidence_pct` 100 from the client — the Project's Entity, or its client DRI —
to a party the Project has first recorded as a [Project Reference](project-reference.md). Its `source_id` is the
[Relationship Log](relationship-log.md) row the write appends on the Project, so the edge's provenance names the matter
that lived the adversity.

**Scope, the target design.** Every Relationship Edge, and every [Relationship Log](relationship-log.md) row, carries
three anchors:

- the [Project](project.md) it was learned in, which is optional;
- the attorney who recorded it, resolved from the signed-in session and never taken from a flag; and
- the [Firm](firm.md).

The edge appears in all three conflict graphs: the Project graph, the attorney's graph, and the Firm graph. Scope
records where a fact was learned. It never hides the fact from a broader scope, so an edge learned in one Project is
still in the attorney's and the Firm's graphs.

Code does not yet do this. Today the `relationship` table carries none of the three anchors (its provenance is
`source_kind` and `source_id`), and a Relationship Log row carries one `graph_scope` (`project`, `attorney`, or `firm`)
rather than all three.

- Schema and queries: [`store::relationships`](../../store/src/relationships.rs) — Lives in the
  `relationship` relation

The table is `relationship`.

Schema: [`relationship`](../../store/postgres/schema.sql).
