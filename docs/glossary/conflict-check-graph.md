---
title: "Conflict-Check Graph"
description: >-
  A Conflict-Check Graph is the graph the firm checks before opening a matter to identify conflicts with current
  clients.
---

The graph the firm walks **before opening a matter** to decide whether the new engagement would conflict with a client
it already serves. Every node is a [Person](person.md) or an [Entity](entity.md); every edge is a typed relationship
between two of them.

The PostgreSQL model holds `entity_role` structural ties and `relationship` supplemental typed edges. Apache AGE
traverses their `navigator_graph` projection when graph authority is `age`. The domain accessors are
[`store::entity_roles`](../../store/src/entity_roles.rs) and [`store::relationships`](../../store/src/relationships.rs).
Authority selection and cutover verification live in the [AGE graph runbook](../age-graph-runbook.md).

A matter writes into the graph through a [Project Reference](project-reference.md): it first references the counterparty
it verified, then records adversity against that referenced party with `POST
/app/api/projects/{id}/relationships/adverse` — a `manual` `adverse_to` edge at full confidence, sourced to the
Relationship Log row the write appends on the Project. The traversal does not read `project_reference` itself; a
reference is how a counterparty gets a node without getting a lens.

The engine collects edges within three hops of the anchors. Rust scores the confidence product along each path and
applies the review and block floors. The traversal is read-only.

A check anchors on the proposed client and entity and surfaces every distinct firm-served party it reaches. It reads
**across matters, unscoped**, because imputed conflicts under Model Rule 1.10 live on other people's matters; the
containment is that only firm-side create paths call it (see
[`access-model`](../access-model.md#where-surrealdb-authorization-lives)). Findings are **advisory to clear,
authoritative to block**: a confident, direct `adverse_to` link to a current client hard-stops the open; softer
entanglements (a shared entity, a recorded [Disclosure](disclosure.md)) are flagged for authorized lawyers to
acknowledge — recorded to the [Relationship Log](relationship-log.md) when they do. The graph can *raise* a conflict;
only a person can *clear* one, because it is never assumed complete.

It runs on every create path (portal, [Navigator MCP](navigator-mcp.md) MCP tool, CLI); the non-interactive paths have
no acknowledgment seam, so any finding refuses the open and routes lawyer to the portal.

A [Matter Page](matter-page.md) draws one matter's slice of the graph through the `relationship_graph`
[Section](section.md): the matter's client Entity, participants, and Project References as nodes, and only the edges
among them (`store::conflicts::edges_among`), as server-rendered SVG with a table of every edge beneath it. It is the
firm's conflicts record, so only the lawyer [Lens](lens.md) carries it.

- Engine: [`store::conflicts`](../../store/src/conflicts.rs), which traverses the resident graph. See
  [multi-cloud](../multi-cloud.md) for the deployment shape.
