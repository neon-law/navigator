---
title: "Project Reference"
description: "A Project Reference names a Person or Entity a Project verified without granting them any standing on it."
---

A [Project](project.md) naming a [Person](person.md) or an [Entity](entity.md) it has verified — a counterparty's
officer, its registered agent, the signatory on the instrument, the adverse party, or the counterparty organization
itself — with **no standing on the matter attached**. PostgreSQL's `project_reference` row links the Project to a typed
Person or Entity and records a `reason` (`counterparty_officer`, `registered_agent`, `signatory`, `adverse_party`, or
`counterparty_entity`).

A reference is deliberately not a [Person–Project Role](personproject-role.md). Every [participation](participation.md)
value grants a lens on the matter, and a counterparty must never have one; yet the [Conflict-Check
Graph](conflict-check-graph.md) must be able to collide a future matter with exactly these people. So the row lives in
its own relation. `store::access` and the embedded Rego policy never read it. The lawyer party roster lists references
under "Referenced, no standing on the matter"; the client lens never loads or renders them. The participation ledger and
the matter workbench's "Project people" list exclude references. Tests cover the access boundary and roster lenses.

It is the one seam through which a Project repository records a counterparty. The `project_reference` model of
`navigator site import` (`seeds/ProjectReference.yaml`) creates the referenced Person or Entity when absent and writes
the reference; a Project-scoped CI session may create an Entity through this model and no other, and only as the
referenced party. `navigator project reference add` sends the same one-record document.

Adversity is recorded against a referenced party: `POST /app/api/projects/{id}/relationships/adverse` (`navigator
project adverse add`) lands a `manual`, full-confidence `adverse_to` [Relationship Edge](relationship-edge.md) from the
client — the Project's Entity, or its client DRI — to the party, sourced to the [Relationship Log](relationship-log.md)
row the write appends on the Project. Given what a reference needs, the verb records the `adverse_party` reference
first. The audit entry, relationship row, and AGE edge commit in one PostgreSQL transaction.

Each `reason` maps to the [Relationship Edge Kind](relationship-edge-kind.md) that describes the tie it records:

- `adverse_party` → [`adverse_to`](adverse-to-edge.md), from the client to the party.
- `counterparty_officer` → [`employs`](employs-edge.md), from the counterparty Entity to its officer.
- `counterparty_entity` → the edge kind that describes the specific tie, such as `retains`, `vendor_of`, or
  `adverse_to`. The reason is a placeholder until that kind is recorded.
- `registered_agent` → [`agent_of`](agent-of-edge.md), from the agent to the Entity it serves.
- `signatory` → [`agent_of`](agent-of-edge.md), from the signatory to the party it signs for.

Only `adverse_party` writes its edge today, through `navigator project adverse add`; the other reasons record the
reference alone.

- Schema and commands: [`store::project_references`](../../store/src/project_references.rs) — Lives in the
  `project_reference` relation

The table is `project_reference`.

Schema: [`project_reference`](../../store/postgres/schema.sql).
