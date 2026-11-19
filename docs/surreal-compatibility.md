# SurrealDB compatibility

PostgreSQL is the application storage authority. Ordinary accessors require PostgreSQL configuration and completed
historical enrollment; a missing connection or invalid enrollment returns an error. They do not select SurrealDB when
PostgreSQL is unavailable. [Storage models and writes](storage-models.md) describes the domain accessors.

SurrealDB remains an explicit source for historical import, compatibility diagnostics, and recovery exports. Keep those
paths until data reconciliation and recovery acceptance permit retirement. A PostgreSQL schema or matching row count
alone does not prove that historical data was transferred.

## Schema

`store/src/schema/navigator.surql` is the compatibility schema: one idempotent definition file applied whole by
`store::schema::apply` in explicit compatibility operations and fixtures. Ordinary application startup does not connect
to SurrealDB, apply that schema, or automatically import its Firm journal. The `schema_version` marker records the build
that applied the definitions; existing rows need explicit, guarded backfills. Follow [SurrealDB
migrations](../.agents/skills/surreal-migrations/SKILL.md) for live compatibility schema changes. PostgreSQL uses its
own ordered SQLx migrations and `_sqlx_migrations` ledger.

Explicit source operations use `NAVIGATOR_SURREAL_ENDPOINT`, `NAVIGATOR_SURREAL_NAMESPACE`, and
`NAVIGATOR_SURREAL_DATABASE`, with their source credentials. Ordinary application startup does not require these
settings. Compatibility fixtures are described in [test database](test-database.md). Database table permissions are
`NONE`; application authorization is described in
[access-model.md](access-model.md#where-surrealdb-authorization-lives).

## People

`store::persons` reads and writes PostgreSQL through `store::postgres::people`. Person and mailbox claims commit
together; aliases, merges, phones, and identity detail retain their PostgreSQL transaction boundaries. Ordinary writes
do not project compatibility rows into SurrealDB.

Historical People imports use the explicit source context and exact Identity enrollment. Import must preserve native
rows and mailbox ownership; source drift or unresolved references block activation. Source selection is confined to the
explicit importer context.

## Relationship Log

`store::relationship_logs` appends to PostgreSQL. Its actor and Project references must exist there, and entries remain
append-only. The Relationship Log is distinct from the AGE edge projection and conflict traversal. Historical source
rows remain subject to the explicit import and containment checks; enabling AGE alone does not prove their transfer.

## Model references and coverage

The glossary links PostgreSQL definitions and canonical accessors. The `glossary_table_coverage` test requires one
owning definition per PostgreSQL table. Compatibility-only records carry explicit SurrealDB compatibility attributions.
`draft` is a compatibility view; its PostgreSQL model is `template`. `schema_version` is the compatibility marker.

`store/postgres/surreal-source-erd.mmd` describes the source schema for migration work. The glossary links an SVG
generated from `store/postgres/postgres-erd.mmd`. Explicit mirror diagnostics and backfill retain their source contracts
in [PostgreSQL tooling](postgres-tooling.md). They cannot overwrite or delete activated PostgreSQL rows to repair them
from an older source. [Surreal archives](surreal-archives.md) covers the retained source export; retiring that backup
also requires recovery evidence.
