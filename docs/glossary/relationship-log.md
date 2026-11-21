---
title: "Relationship Log"
description: "A Relationship Log is an append-only audit trail of relationship changes."
---

Append-only audit trail of relationship changes — entries like `person joined entity` or `project closed`. The source of
truth for "what changed when" outside of normal table rows.

A Relationship Log entry records an actor’s action on a subject. Explicit authorized commands create typed [Relationship
Edges](relationship-edge.md) between two endpoints in the [Conflict-Check Graph](conflict-check-graph.md). Automation
records identity and log entries without creating typed edges.

The relational model is the PostgreSQL `relationship_log` table. Its append path is described in [Storage
models](../storage-models.md#relationship-graph).

An operator appends rows through the `relationship_log` model of `navigator site import`, keyed by the subject Person's
email, the action, and the detail, so re-importing an identical row appends nothing. `navigator project docket ingest`
writes one row per person per filing from a court notice's [Service List](service-list.md).

A row may name the [Project](project.md) whose work produced it in `project_id`, and `for_project` reads a Project's
rows newest first. The Project scopes the row; it grants no role on it. A Project-scoped seed session stamps its own
Project and refuses a row naming another; `docket ingest` stamps the checkout's Project.

A signed-in lawyer can record an observation about an existing Person or Entity with `navigator relationship add`.
Project scope uses `--project <code>` or the current Project checkout. Attorney and Firm scopes require `--firm <id>`.
Supply `--person <id>` or `--entity <id>`, a type such as `witness` with `--type`, and `--detail <text>`. The site
records the signed-in author and checks Firm membership and Project participation. Use `--request-id <uuid>` to retry
the same observation without appending it twice.

The Project graph shows the Project's observations, the lawyer's own graph shows their Project and attorney
observations, and the Firm graph shows its Project and Firm observations to Firm members. Graph views show the subject
and type. The audit entry holds the detail. A logged observation grants no participation.

- Schema and queries: [`store::relationship_logs`](../../store/src/relationship_logs.rs) — Lives
  in: `relationship_log` table

The table is `relationship_log`.

Schema: [`relationship_log`](../../store/postgres/schema.sql).
