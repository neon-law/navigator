# PostgreSQL access and migrations

Navigator uses SQLx 0.9 with Tokio and Rustls for PostgreSQL access. The Firm journal uses the same driver. Axum
handlers share a bounded pool; credentials are discrete configuration values and the deployed connection requires
`verify-full` TLS. `store/tests/postgres_tooling.rs` exercises the driver and SQLx's timestamped migration runner
against the pinned database image.

SQLx's built-in `Migrator` is the application migration runner. It orders `YYYYMMDDHHMMSS_description.sql` files,
records checksums in `_sqlx_migrations`, uses PostgreSQL advisory locking for concurrent runners, and applies each
ordinary migration in a transaction. Run it as an explicit operator action, before a service starts accepting requests.
A web request must never apply DDL.

`PostgresDb::migrate_directory` runs on a dedicated pool connection and closes it after success or failure. SQLx's error
path can retain a session advisory lock; returning that connection to the application pool would block a later runner.
The operator path refuses empty directories, nontransactional migrations and standalone `BEGIN`/`COMMIT`/`ROLLBACK`
lines before executing any SQL. The connection retains its pool permit and closes on drop, including cancellation, so
migration concurrency cannot bypass the configured connection limit.

| Contract | Direct `raw_sql` execution | SQLx `Migrator` |
| --- | --- | --- |
| Fresh install | Executes the supplied SQL | Orders and applies pending migrations |
| Upgrade | Caller must decide what is pending | Uses the recorded version history |
| Checksum drift | Has no applied-byte comparison | Refuses an edited applied migration |
| Failure recovery | Caller owns transaction boundaries | Rolls back ordinary DDL and its version record |
| Concurrent runners | Caller must acquire and release a lock | Serializes on the database migration lock |

The test runs two migration runners concurrently, verifies each version and seed effect occurs once, upgrades the
schema, refuses checksum drift, and proves failed DDL leaves neither its table nor a successful version record. It also
round-trips UUID, timezone-aware timestamp and JSONB values and checks transaction rollback and the configured pool
limit. SQLx already provides the required driver and runner; another database ORM or migration dependency would
introduce a second access contract.

## The operator migration command

`navigator-migrate` (the `store` crate's binary) is the one operator entrypoint. It compiles in the checked-in
`store/postgres/migrations`, so a Kubernetes Job needs no source checkout; the `neon-server` release image
(`images/Containerfile.neon`) carries it at `/app/navigator-migrate`. It reads the same discrete `NAVIGATOR_POSTGRES_*`
values as the application (`verify-full` TLS; never the bootstrap superuser), applies every pending migration through
`PostgresDb::migrate_embedded`, prints the last applied version and exits nonzero on any refusal.

```bash
cargo run -p store --bin navigator-migrate                          # every pending migration
cargo run -p store --bin navigator-migrate -- --through <version>   # pending migrations up to <version>
cargo run -p store --bin navigator-migrate -- --dir <path>          # a directory instead of the compiled set
```

Run it as a Job, with a migration role's credentials, and start services only after it succeeds. Every migration lets
SQLx own its transaction, lock and `_sqlx_migrations` row: no file contains `BEGIN`/`COMMIT` or writes a ledger of its
own.

Startup verifies application admission in PostgreSQL before serving requests. The Firm-authorized Journal
migration-triage endpoint reports outstanding work for an admitted database.

## Adopting a database installed from `schema.sql`

A database built by piping `schema.sql` through `psql` has the tables but no `_sqlx_migrations` rows, so the runner
refuses it at the first `CREATE TABLE`. Never infer an applied version or checksum from a matching table name. Adopt it
only by proving its schema identical to a runner-built reference, on the same server and image, before any write:

1. Find the newest migration the install contains. Build a scratch reference database with that release's binary:
   `navigator-migrate --through <version>` with `NAVIGATOR_POSTGRES_DATABASE` set to the reference.
2. Dump both schemas with the same arguments and compare them. Any difference stops the adoption.

   ```bash
   pg_dump --schema-only --no-owner --no-privileges --restrict-key=adoption \
     --exclude-table=_sqlx_migrations --exclude-table=schema_migrations <database> > <database>.sql
   diff target.sql reference.sql
   ```

3. Dump the reference ledger with `pg_dump --no-owner --no-privileges --restrict-key=adoption
   --table=_sqlx_migrations <reference> > ledger.sql`. Then, in one transaction on the target, refuse a ledger that
   already has rows, retire the superseded `schema_migrations` table and load the copied ledger:

   ```sql
   DO $$ BEGIN
       IF to_regclass('public._sqlx_migrations') IS NOT NULL AND EXISTS (SELECT 1 FROM _sqlx_migrations) THEN
           RAISE EXCEPTION 'target already records SQLx migrations; do not adopt';
       END IF;
       IF to_regclass('public.schema_migrations') IS NOT NULL AND EXISTS (
           SELECT 1 FROM schema_migrations WHERE version <> '20261011200000_create_firm_journal.sql') THEN
           RAISE EXCEPTION 'unexpected schema_migrations rows; do not adopt';
       END IF;
   END $$;
   DROP TABLE IF EXISTS _sqlx_migrations;
   DROP TABLE IF EXISTS schema_migrations;
   ```

   Save that as `adopt.sql` and run `psql -v ON_ERROR_STOP=1 --single-transaction -f adopt.sql -f ledger.sql <target>`.
4. Run `navigator-migrate` against the target. It verifies every copied checksum against the compiled SQL and applies
   only the later migrations. Drop the reference database.

`store/tests/postgres_tooling.rs` proves the fresh install, the idempotent re-run, the journal migration under the
runner, and that a `schema.sql` install runs only after its catalog matches the reference and the ledger is copied.

AGE's `agtype` and pgvector are extension types. SQLx can send a parameterized query and read an explicit text/JSON
projection without introducing a second driver. Keep their application representations explicit; a passing UUID test
does not establish an extension decoder contract. The integrated compatibility tests cover those projections separately.

## Application admission

The application checks its Database Origin or completed historical PostgreSQL receipts before admitting application
traffic. The [storage contract](storage-models.md#historical-admission) describes both forms of evidence.
