# Archives

`archives` is the durable export service for Navigator's operational data. It snapshots SurrealDB tables to Parquet,
records cloud-cost data when configured, and reports each scheduled run to firm operators.

It serves deployment operators and analysts who need recoverable, queryable history without placing reporting work on
the application database. Restate hosts the workflow in `workflows-service`, so retries and partial failures do not
require a separate long-running archive service.

See [Iceberg archive](../docs/iceberg-archive.md) for the data contract, [scheduled jobs](../docs/cronjobs.md) for the
runtime shape, and the crate source for implementation details.

The cloud and KIND CronJobs both fire at **05:55 and 17:55 UTC**. The trigger sends the scheduled UTC time as the
workflow key and request payload; retries within that run attach to the same Restate invocation. Application Parquet
uses `application/<table>/data/dt=<date>/run=<HHMM>/snapshot.parquet`, with `0555` and `1755` as separate generations.
Re-extraction overwrites the same generation file, and Iceberg promotion selects only that generation's files.

A missed scheduled run can be submitted with `ARCHIVES_SCHEDULED_FOR=<RFC3339 UTC time>` on the trigger. It must name
05:55 or 17:55 exactly. The override preserves that run's key and date; it does not reconstruct historical source data.

The journaled `email` step posts mrkdwn through the existing Slack bot to the deployment's messages channel (the staging
messages channel for simulated matters). Missing bot credentials or a channel fail the step rather than silently
capturing a successful digest. Slack transport failures retry that step; acknowledgment loss can produce a duplicate
notice. The durable step names remain unchanged so existing invocations can replay.

When `NAVIGATOR_POSTGRES_HOST` is set, the snapshot step requires reviewed coverage for Navigator and Forgejo. Both
databases must export successfully before a paired completion manifest is written. Each retains its own MVCC watermark.
Missing inventory, inaccessible relations, schema drift, or failed Iceberg metadata publication records a PostgreSQL
failure in the digest. The SurrealDB snapshot has its own outcome. See PostgreSQL coverage below.

`NAVIGATOR_ARCHIVE_SOURCE=postgres` selects PostgreSQL-only extraction without opening a SurrealDB connection. It
requires `NAVIGATOR_POSTGRES_HOST`; paired coverage, reader credentials and TLS configuration remain mandatory. The
default, or `all`, retains SurrealDB extraction and any configured PostgreSQL export. Other values fail. Select
PostgreSQL-only extraction only after reconciling the authoritative data; it does not export rows still held solely in
SurrealDB. The combined Restate worker still needs SurrealDB for its other services.

A PostgreSQL failure is journaled for digest delivery and terminates that invocation. Repair its configuration or source
and start a new invocation; replay retains the failed extraction outcome.

Iceberg publication refuses unsupported Arrow types and selected Parquet files with different schemas. The persistent
`postgres/navigator/<table>` lineage preserves field IDs when the same live PostgreSQL relation adds, renames, drops, or
reorders columns; new fields get IDs above the prior maximum. It fails closed when relation identity is missing or
changed, metadata is malformed, or a continuing field has an incompatible mapped type change. This continuity does not
cover dump/restore or OID reuse. Paired PostgreSQL coverage uses a separate generation-scoped lineage. Other archive
sources retain their existing fail-closed schema behavior.

The `SurrealDB` lane writes full-table application snapshots, not a database-wide consistent recovery image. Empty-table
replacement, concurrent generation publication, cross-project archive infrastructure and native DBOS orchestration
remain separate migration gates. A merged schedule change requires a published release and worker/trigger rollout before
it is live.

## PostgreSQL coverage

Configured PostgreSQL runs require `NAVIGATOR_ARCHIVE_COVERAGE`, a JSON object containing `navigator` and `forgejo`
coverage manifests. Each manifest has a `relations` array of exact schema/name/kind rules. `export` rules include
`accessible: true`; `derived` rules include an ordinary view and a nonempty reconstruction reason. Review inventories
against the database's owned schema. Capture catalog input with `coverage::inventory`; accepting every discovered
relation automatically would hide drift.

The Forgejo source uses the same PostgreSQL server and TLS options with database `forgejo` and separate
`NAVIGATOR_ARCHIVE_FORGEJO_USER` and `NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD` credentials. The caller must provision read
access to all covered application, DBOS, AGE, migration-ledger and large-object relations. Missing configuration or
denied reads fail PostgreSQL coverage. This crate does not modify grants or service-owned schemas.

`coverage::export_pair` extracts both sources before uploads, authors Iceberg metadata through the existing writer, then
writes `postgres/_paired/<generation>.json` through `cloud::StorageService`. Readers require that final marker and
verify each Parquet hash. Source objects and metadata under an unfinished generation are uncommitted. Each source
records its own database name, MVCC watermark, cutoff, column types, and non-MVCC sequence relations. Paired coverage
does not certify cross-database atomicity or a matching repository/document-byte recovery point.

The synthetic drill in `tests/coverage_postgres.rs` uses separate source and restore databases, reads through Iceberg,
applies each source's schema procedure, and compares row hashes. Run it with `NAV_REQUIRE_POSTGRES=1` and
`NAVIGATOR_TEST_POSTGRES_PORT` set to this worktree's PostgreSQL fixture port. It proves the tested schema shapes; full
Forgejo service boot, DBOS replay, coordinated recovery points, and external-byte restoration require their own drills.
See [Iceberg Archive](../docs/glossary/iceberg-archive.md).

For the full pinned Forgejo fixture, set `NAVIGATOR_TEST_FORGEJO_PRE_SCHEMA`, `NAVIGATOR_TEST_FORGEJO_DATA`, and
`NAVIGATOR_TEST_FORGEJO_POST_SCHEMA` to local `pg_dump` sections from this worktree's quiesced synthetic Forgejo. Use
`--no-owner --no-privileges`; the data section also needs `--inserts`. All three inputs are required together. The drill
checks the 128-table service schema, inserts synthetic repository metadata, restores rows through Iceberg, and applies
Forgejo's post-data constraints. Keep generated fixture dumps private and outside Git.
