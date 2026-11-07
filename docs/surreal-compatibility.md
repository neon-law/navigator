# SurrealDB compatibility

PostgreSQL is the model for new storage work. This page holds the engine-specific contracts that remain necessary while
domain accessors move to PostgreSQL. [Storage models and writes](storage-models.md) identifies supported write authority
choices. Keep these compatibility paths until their callers have a verified replacement.

## Schema

`store/src/schema/navigator.surql` is the Statement of the present: one idempotent definition file applied whole by
`store::schema::apply`. Its `schema_version` marker records which build applied it. Definitions converge on apply;
existing rows need explicit, guarded backfills. Follow [SurrealDB
migrations](../.agents/skills/surreal-migrations/SKILL.md) for live compatibility schema changes. PostgreSQL uses its
own ordered SQLx migrations and `_sqlx_migrations` ledger.

The connection contract uses `NAVIGATOR_SURREAL_ENDPOINT`, `NAVIGATOR_SURREAL_NAMESPACE`, and
`NAVIGATOR_SURREAL_DATABASE`. Embedded tests and the local KIND dependency are described in [test
database](test-database.md). Database table permissions are `NONE`; application authorization is described in
[access-model.md](access-model.md#where-surrealdb-authorization-lives).

## People

Without PostgreSQL People authority, `store::persons` writes SurrealDB. A normalized `person_mailbox` record key
serializes competing claims; a unique email index supplies a backstop. Alias writes and Person merges use this path and
are refused under PostgreSQL People authority. PostgreSQL People writes project compatibility rows after their
transaction commits; this projection is a separate write, not a transaction spanning both engines.

`store::person_phones` writes structured phone rows through SurrealDB and mirrors them through `source_domain_records`.
Its transaction demotes other primary numbers before setting a primary row.

## Relationship Log

`store::relationship_logs` appends to SurrealDB. Graph authority does not move this writer. PostgreSQL's
`relationship_log` table describes its relational model; enabling AGE does not transfer the log's append authority.

## Model references and coverage

The glossary links PostgreSQL definitions and canonical accessors. The `glossary_table_coverage` test requires one
owning definition per PostgreSQL table. Compatibility-only records carry explicit SurrealDB compatibility attributions.
`draft` is a compatibility view; its PostgreSQL model is `template`. `schema_version` is the compatibility marker.

`store/postgres/surreal-source-erd.mmd` describes the source schema for migration work. The glossary links an SVG
generated from `store/postgres/postgres-erd.mmd`. Mirror reconciliation, backfill, and shadow reads retain their
SurrealDB source contracts in [PostgreSQL tooling](postgres-tooling.md). [Surreal archives](surreal-archives.md) covers
the source export.
