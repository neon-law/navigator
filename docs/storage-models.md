# Storage models and writes

Use PostgreSQL models for new storage work. Name tables from the [glossary](glossary/README.md), use relational
constraints, and add ordered migrations under `store/postgres/migrations/`. The consolidated model is
[`store/postgres/schema.sql`](../store/postgres/schema.sql); SQLx owns the migration ledger. Apply migrations with
`navigator-migrate` before serving requests. [PostgreSQL tooling](postgres-tooling.md) covers the driver and runner.

Write through the domain accessor. Prefer its PostgreSQL path where supported and configured. A mirror records a copy;
enabling mirroring alone does not transfer write authority. Keep the active accessor until its domain has a verified
cutover, including parent rows, parity, and unsupported operations.

## People

`store::persons` selects PostgreSQL when `NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY` is `1` or `true`. Person and mailbox
claims commit together through `store::postgres::people`; successful writes project compatibility rows for callers whose
other records still use those links. A configured PostgreSQL failure returns an error. It does not fall back to another
writer. Alias writes and Person merges are refused under PostgreSQL People authority. The people directory reads the
`person_directory` and `firm_visible_person` views in one query; without the authority it reads people, memberships and
Firms once each.

## Relationship graph

`NAVIGATOR_GRAPH_AUTHORITY=age` selects PostgreSQL relational edge writes and Apache AGE traversal of `navigator_graph`.
Follow the [AGE graph runbook](age-graph-runbook.md) for extension setup, migrations, backfill, parity, and cutover.
After PostgreSQL graph writes begin, the SurrealDB graph is a frozen snapshot; changing the reader back cannot supply
fresh graph data. The Relationship Log has a separate append contract.

## Project lifecycle

After applying the Project Status Transition migration, set `NAVIGATOR_POSTGRES_PROJECT_LIFECYCLE_AUTHORITY=1` on all
lifecycle writers together. PostgreSQL commits each [Project Status Transition](glossary/project-status-transition.md)
with the Project's current status and retention start. The existing Project mirror supplies the parent row and the first
transition records the known starting state. Status reads use the PostgreSQL projection for enrolled Projects. The
lifecycle command also refreshes the SurrealDB compatibility projection. If that refresh fails, retry the same
transition to repair it. Keep lifecycle authority enabled once a deployment has recorded transitions.

## Matter records

`NAVIGATOR_POSTGRES_MATTER_AUTHORITY=1` (or `true`) routes the owning modules of `statutory_deadline`, `notation_draft`,
`estate_asset`, `cap_table_holding`, `fiduciary_role`, and `git_repository` to PostgreSQL through
`store::postgres::matter`, a generic accessor over the records mirror's table registry. Writes land only in PostgreSQL.
The records mirror then refuses to repair those tables from SurrealDB, because a missing SurrealDB row is not deletion
evidence. Reference checks still read the Project, Person, Entity, and Notation from their own authorities, and the
Project mirror supplies the parent rows. Run the records reconcile and resolve drift before enabling the switch. Keep it
enabled once a deployment has written matter records.

## Other domains

The [Firm journal](firm-journal.md) documents its backend selection and PostgreSQL-only reconciliation features.
[Tasks](glossary/task.md), [Case Stages](glossary/case-stage.md), and [Purchases](glossary/purchase.md) have
PostgreSQL-native models. Their referenced parent rows must exist in PostgreSQL before a write can succeed.

For domains still using the compatibility engine, retain their accessor and migration safeguards. See [SurrealDB
compatibility](surreal-compatibility.md) and the [mirror coverage
inventory](postgres-tooling.md#postgresql-mirror-tables-and-reconciliation). A PostgreSQL schema link in the glossary
describes a model, not a claim that every deployment reads and writes that table there.
