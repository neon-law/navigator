---
title: "Relationship Edge"
description: "A Relationship Edge is a typed graph connection between a Person or Entity on each end."
---

A typed graph edge with a [Person](person.md) or [Entity](entity.md) on **each** end — the canonical two-sided
relationship the [Conflict-Check Graph](conflict-check-graph.md) traverses. Distinct from the [Relationship
Log](relationship-log.md), which is a one-sided audit trail (one actor, one subject); a Relationship Edge instead
asserts "A is `adverse_to` B" or "A is a `related_party` of B."

Each edge carries provenance (`source_kind` ∈ `manual` / `disclosure` / `relationship_log` / `llm`) and a
`confidence_pct` (0–100). Human-asserted edges are full confidence; edges an LLM parses out of a Relationship Log's
free-form detail land lower, and the conflict check multiplies confidence along a path so a chain of weak guesses cannot
raise a finding on its own.

Both endpoints are native `record<person|entity>` links, so an endpoint-kind typo cannot be written at all.

A matter asserts adversity through `POST /app/api/projects/{id}/relationships/adverse` (`navigator project adverse
add`): a `manual` `adverse_to` edge at `confidence_pct` 100 from the client — the Project's Entity, or its client DRI —
to a party the Project has first recorded as a [Project Reference](project-reference.md). Its `source_id` is the
[Relationship Log](relationship-log.md) row the write appends on the Project, so the edge's provenance names the matter
that lived the adversity.

- Schema and queries: [`store::relationships`](../../store/src/relationships.rs) — Lives in the
  `relationship` relation
