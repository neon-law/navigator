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
writer. Alias emails are mailbox claims, and a Person merge moves every PostgreSQL row that links the dropped Person in
one transaction after the SurrealDB records move. The people directory reads the `person_directory` and
`firm_visible_person` views in one query; without the authority it reads people, memberships and Firms once each.

The same switch moves identity detail to PostgreSQL through `store::postgres::identity`: addresses, credentials, email
tokens, entities, Person delegates, external identities, and Person phones. Each committed write is projected into
SurrealDB with any Firm anchor claim. Under the switch the legacy mirror never copies these rows back into PostgreSQL.

## Relationship graph

Wherever PostgreSQL is configured it holds the graph tables: `entity_role`, `relationship`, `project_reference`,
`disclosure`, and the append-only `relationship_log`. Their writes and reads run on PostgreSQL, the edges land in
`navigator_graph`, and the conflict check traverses it in Apache AGE. The mirror backfill copies their Surreal-era rows
across but never deletes one PostgreSQL holds. A deployment without PostgreSQL keeps them in SurrealDB. Follow the [AGE
graph runbook](age-graph-runbook.md) for extension setup, migrations, backfill, parity, and cutover.

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

## Communications and email

Whenever `NAVIGATOR_POSTGRES_HOST` is set, `store::communications`, `store::email_conversations`,
`store::email_receipts`, and `store::email_deliveries` read and write PostgreSQL only: the `communication`,
`email_conversation`, `email_conversation_message`, `email_receipt`, `email_delivery`, and `email_delivery_attempt`
tables. The mirror neither copies nor compares those tables. Without a host, SurrealDB is the fallback. The unique
indexes decide replay races, and a delivery's reservation commits with its attempt. Parent rows (Project, Person, Firm,
Notation, Asset) must exist in PostgreSQL. A retention purge refuses while an Inbox Message or a Notation source still
names a Communication it would delete. The Inbox tables follow the same rule.

## Other domains

The [Firm journal](firm-journal.md) documents its backend selection and PostgreSQL-only reconciliation features.
[Tasks](glossary/task.md), [Case Stages](glossary/case-stage.md), and [Purchases](glossary/purchase.md) have
PostgreSQL-native models. Their referenced parent rows must exist in PostgreSQL before a write can succeed.

For domains still using the compatibility engine, retain their accessor and migration safeguards. See [SurrealDB
compatibility](surreal-compatibility.md) and the [mirror coverage
inventory](postgres-tooling.md#postgresql-mirror-tables-and-reconciliation). A PostgreSQL schema link in the glossary
describes a model, not a claim that every deployment reads and writes that table there.
