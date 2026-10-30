---
title: "Project Reference"
description: "A Project Reference names a Person or Entity a Project verified without granting them any standing on it."
---

A [Project](project.md) naming a [Person](person.md) or an [Entity](entity.md) it has verified — a counterparty's
officer, its registered agent, the signatory on the instrument, the adverse party, or the counterparty organization
itself — with **no standing on the matter attached**. It is a relation, `RELATE
project->project_reference->(person|entity)`, carrying only a `reason` (`counterparty_officer`, `registered_agent`,
`signatory`, `adverse_party`, or `counterparty_entity`).

A reference is deliberately not a [Person–Project Role](personproject-role.md). Every [participation](participation.md)
value grants a lens on the matter, and a counterparty must never have one; yet the [Conflict-Check
Graph](conflict-check-graph.md) must be able to collide a future matter with exactly these people. So the row lives in
its own relation, and three things are asserted by tests rather than by convention: `store::access` and the embedded
Rego policy never read it, and a party list — the participation ledger a Project portal reads, and the matter
workbench's "Project people" — never renders it.

It is the one seam through which a Project repository records a counterparty. The `project_reference` model of
`navigator site import` (`seeds/ProjectReference.yaml`) creates the referenced Person or Entity when absent and writes
the reference; a Project-scoped CI session may create an Entity through this model and no other, and only as the
referenced party. `navigator project reference add` sends the same one-record document.

Adversity is recorded against a referenced party: `POST /app/api/projects/{id}/relationships/adverse` (`navigator
project adverse add`) lands a `manual`, full-confidence `adverse_to` [Relationship Edge](relationship-edge.md) from the
client — the Project's Entity, or its client DRI — to the party, sourced to the [Relationship Log](relationship-log.md)
row the write appends on the Project.

- Schema and commands: [`store::project_references`](../../store/src/project_references.rs) — Lives in the
  `project_reference` relation

The table is `project_reference`.

Schema: [`project_reference`](../../store/src/schema/navigator.surql#L1187).
