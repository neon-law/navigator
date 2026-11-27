# Archives

The `archives` crate exports PostgreSQL tables to Parquet, publishes Iceberg metadata, and reads retained generations
for recovery and verification. Operators run logical export and recovery through [Hudson](../docs/iceberg-archive.md).
Physical PostgreSQL backups and WAL remain the disaster-recovery path.

The logical exporter requires reviewed coverage, read-only database credentials, TLS, and approved object storage. It
exports Navigator and Forgejo before committing the paired generation. See the [write-only
archive](../docs/write-only-iceberg.md) for retention-locked destinations and verification.

## PostgreSQL coverage

Configured PostgreSQL runs require `NAVIGATOR_ARCHIVE_COVERAGE`, a JSON object containing `navigator` and `forgejo`
coverage manifests. Each manifest has a `relations` array of exact schema/name/kind rules. `export` rules include
`accessible: true`; `derived` rules include an ordinary view and a nonempty reconstruction reason. Review inventories
against the database's owned schema. Capture catalog input with `coverage::inventory`; accepting every discovered
relation automatically would hide drift.

The reviewed manifest lives in `k8s/postgres/archive/coverage.json`, which `ops ship` renders as the
`navigator-archive-coverage` ConfigMap. `tests/coverage_manifest.rs` holds its `navigator` half to a freshly migrated
database with DBOS launched. `cargo run -p archives --example coverage_inventory -- <url>` prints a review draft of any
database, which is how the `forgejo` half is captured. See [Iceberg
archive](../docs/iceberg-archive.md#exporting-a-generation).

The export uses a read-only database login for each source. Physical backups and WAL provide disaster recovery.

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
applies each source's schema procedure, and compares row hashes. Run it with `NAVIGATOR_TEST_POSTGRES_PORT` set to this
worktree's PostgreSQL fixture port. It proves the tested schema shapes; full Forgejo service boot, DBOS replay,
coordinated recovery points, and external-byte restoration require their own drills. See [Iceberg
Archive](../docs/glossary/iceberg-archive.md).

For the full pinned Forgejo fixture, set `NAVIGATOR_TEST_FORGEJO_PRE_SCHEMA`, `NAVIGATOR_TEST_FORGEJO_DATA`, and
`NAVIGATOR_TEST_FORGEJO_POST_SCHEMA` to local `pg_dump` sections from this worktree's quiesced synthetic Forgejo. Use
`--no-owner --no-privileges`; the data section also needs `--inserts`. All three inputs are required together. The drill
checks the 128-table service schema, inserts synthetic repository metadata, restores rows through Iceberg, and applies
Forgejo's post-data constraints. Keep generated fixture dumps private and outside Git.
