---
title: "Agent Of Edge"
description: >-
  An Agent Of Edge records that one party acts on another's behalf with authority, such as a registered agent.
---

The Relationship Edge kind `agent_of`: A acts on B's behalf with authority — a registered agent, a collector, or a
signatory. Where B engaged A to act, a [Retains Edge](retains-edge.md) runs the other way, from B to A.

- Kind: `agent_of`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the agent) to B (the principal).
- Endpoints: A [Person](person.md) or [Entity](entity.md) on each end.
- Example: A collection agency is agent_of a creditor; a registered-agent company is agent_of the company it serves.
- Retired aliases: The `registered_agent` and `signatory` [Project Reference](project-reference.md) reasons map to
  `agent_of`.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
