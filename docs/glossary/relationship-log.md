---
title: "Relationship Log"
description: "A Relationship Log is an append-only audit trail of relationship changes."
---

Append-only audit trail of relationship changes — entries like `person joined entity` or `project closed`. The source of
truth for "what changed when" outside of normal table rows.

It is **not** the [Conflict-Check Graph](conflict-check-graph.md): a Relationship Log row is one-sided (an actor acted
on a subject), whereas the graph's edges are two-sided [Relationship Edges](relationship-edge.md). The log *feeds* the
graph — an LLM can parse a row's free-form detail into typed edges — and the graph writes back to the log when lawyers
acknowledge a conflict override.

It moved to SurrealDB with the graph because its **writers** did: `store::projects` and `store::project_modules` reached
across engines for this one insert, so a matter open was a two-engine write with no transaction spanning it.

An operator appends rows through the `relationship_log` model of `navigator site import`, keyed by the subject Person's
email, the action, and the detail, so re-importing an identical row appends nothing. `navigator project docket ingest`
writes one row per person per filing from a court notice's [Service List](service-list.md).

A row may name the [Project](project.md) whose work produced it in `project_id`, and `for_project` reads a Project's
rows newest first. The Project scopes the row; it grants no role on it. A Project-scoped seed session stamps its own
Project and refuses a row naming another; `docket ingest` stamps the checkout's Project.

- Schema and queries: [`store::relationship_logs`](../../store/src/relationship_logs.rs) — Lives
  in: `relationship_log` table
