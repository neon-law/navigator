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

Run it as a Job, with a migration role's credentials, and start services only after it succeeds. `hudson journal
migrate` calls the same runner. Every migration lets SQLx own its transaction, lock and `_sqlx_migrations` row: no file
contains `BEGIN`/`COMMIT` or writes a ledger of its own.

Ordinary web and worker startup requires activated PostgreSQL history. It neither opens SurrealDB nor applies schema or
imports Journal Entries. To import one Firm's timestamped Journal Entries, run `hudson journal --firm <uuid>
import-legacy` with operator PostgreSQL and read-only SurrealDB source credentials. The bounded import retains
identities and insertion timestamps, refuses conflicting content, and reports timestamp or reversal triage without
inventing evidence. `journal migration-triage` remains an authenticated, Firm-authorized read of that explicit source.

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

## Template version constraints

Migration `20261016130000` validates the exact Template versions pinned by Notations, Notation Events, and packages. For
an existing PostgreSQL mirror:

1. Run `navigator-migrate --through 20261016123000` to create the Template and email audit tables.
2. Run `hudson ops postgres mirror reconcile` against the saved source versions and resolve remaining drift.
3. Run the complete migration command.

A missing Template version stops the constraint migration transactionally: it leaves every row and all three constraints
unchanged. Repair the source-backed mirror and retry; never invent a version or delete a pin to make the migration pass.

## PostgreSQL mirror tables and reconciliation

Application reads and writes use PostgreSQL. Missing configuration or incomplete authority history stops ordinary
access before a Source connection. [Storage models and writes](storage-models.md) describes native ownership,
authorization, and transaction boundaries.

[`store::postgres_mirror`](../store/src/postgres_mirror.rs) retains explicit captured-history adapters. Global history
and the five disjoint scoped families bind their registered tables to verified captures. A completed receipt fences
Source import and reconciliation before Source access, including under an explicit Source-read context. A missing
native parent after ownership transfer is a refusal, not permission to restore it from old Source data.

| Historical group | Captured behavior |
| --- | --- |
| Identity and detail | Preserve UUIDs, typed fields, and canonical natural mailbox claims without reassignment |
| Tenancy | Preserve ownership and participation; prove native lifecycle chains before accepting divergence |
| Documents | Compare complete Asset metadata and JSON; preserve independently proven native protection |
| Templates | Preserve immutable versions, provenance and pins; never delete target-only versions |
| Workflow | Preserve Notation pins, respondent/actor attribution, answer chronology, and live reset protections |
| Catalogs and graph | Permit genuine first-global copy; completed ownership prevents parent resurrection |
| Brand and social content | Preserve stable content identity, connected accounts, selections, and encrypted context |
| Communications and Inbox | Preserve transcript, scope, visibility, and audit evidence; replay no external effects |
| Application records | Insert captured missing rows; preserve native changes and target-only history |
| Sent Email | Compare retained history; admit missing rows only with the exact proven retention exclusion |
| Journal | Prove every captured event represented and all retained native projections and history coherent |

Comparisons retain canonical columns, nested values, nullable references, and PostgreSQL timestamp precision.
Invalid identities or timestamps fail closed rather than being dropped or assigned substitutes. Encrypted values retain
their exact authenticated context and ciphertext; import does not decrypt or normalize authenticated text. Mailbox
reports contain Person ids and claim counts, never mailbox strings. Other reports contain ids, counts, and sanitized
reasons rather than client content or secret values.

The explicit import command uses the existing enrollment ledger and its captured Source. It does not grant ordinary
application requests an alternative storage path. Pending import policies preserve each family's immutable history,
native ownership exceptions, and unresolved-reference checks; a clean table count alone cannot certify transfer.
See [scoped enrollment](#scoped-family-enrollment) for the verified capture and activation protocol.

Normal deployment runs the PostgreSQL-only application readiness Job before application rollout. It requires global
history and every current scoped receipt. Only successful admission retires the obsolete automatic mirror-recovery
schedule and monitoring rule. Explicit import, archive, and recovery commands remain separate operator actions;
readiness failure leaves recovery intact and blocks rollout.

## Mirror backfill

An unscoped [Mirror Backfill](glossary/mirror-backfill.md) copies the rows SurrealDB holds and PostgreSQL lacks: history
written before a deployment turned the mirror on, or a write whose projection failed and was never repaired. It writes
each row through the same per-row mirror the write hook and `reconcile` use, so a backfilled row is the row the mirror
would have written. It never overwrites a differing row or deletes a PostgreSQL-only row; those stay with `ops postgres
mirror reconcile`.

The first global backfill against a database, before any global run has completed, is also the only global copy into the
tables PostgreSQL holds authoritatively: catalogs, matter records, Brand and social content, communications, Inbox and
email history, and the historical Product catalog. It writes the SurrealDB rows those tables lack and leaves every row
PostgreSQL holds. Its completion lets `ops ship` cut a deployment over. Later runs leave those tables alone, so a row
deleted in PostgreSQL stays deleted.

```bash
hudson ops postgres backfill --dry-run --json          # the plan; writes nothing
hudson ops postgres backfill --dry-run --verify-assets --json  # include referenced objects
hudson ops postgres backfill --batch-size 500 --json   # start a run
hudson ops postgres backfill --max-batches 20 --json   # start a run, stop after 20 batches
hudson ops postgres backfill --resume <run-id> --json  # continue a run from its checkpoints
```

- **Plan.** `--dry-run` compares every mirror in registry order and lists, per table, the rows a run would write (by
  UUID), the batches they need, and the differing and PostgreSQL-only rows it would leave. It writes nothing, not even a
  run record. The same stores give the same plan.
- **Source tables.** The dry-run report includes every live source table, its row count, and its disposition:
  `mirrored`, `born_in_postgres`, `derived`, `recreated_metadata`, or `unsupported`. Empty unsupported tables stay
  visible. Populated unsupported tables block both planning and import. Derived tables rebuild from canonical rows, and
  `schema_version` is recreated by target migrations. Missing inventory metadata fails closed; an empty inventory must
  be explicit.
- **Source fields.** Whole-row record mirrors refuse source fields without a declared preservation mapping, including
  explicit nulls and empty values. The error gives the registered table and field count only. Declared JSON content is
  preserved recursively; absent optional fields retain their null mapping. Consumed source-key metadata supplies the row
  identity, while generated PostgreSQL identity columns and target-only metadata are not source-field mappings.
- **Batches.** A run walks the mirror groups parents first. Each batch writes up to `--batch-size` rows, then advances
  the group's row in `mirror_backfill_checkpoint`. A group is complete once its missing rows are written or recorded.
- **Resume.** `--resume` recomputes the current group's missing rows, so rows an interrupted batch already wrote are
  skipped. Every write is keyed on the row's identity, so a crash between a batch's rows and its checkpoint cannot
  duplicate a row. A completed run resumes as a no-op, and a new run over converged stores writes nothing.
- **Failures.** A row still refused after a reverse-order retry is recorded in `mirror_backfill_failure` by table, UUID,
  and sanitized reason, such as a SQLSTATE. A row keyed by a Brand key is counted, never named.
- **Post-run parity.** Every invocation reports fresh per-table missing, differing, and PostgreSQL-only counts, plus
  up to 50 divergent UUID keys per table. Other key forms and excess UUIDs are counted without being printed. Exit
  status 0 requires all mirrored tables to have zero drift and no failed rows; status 1 reports a completed run whose
  imported data still needs reconciliation. A paused run keeps status 3 and can be resumed.
- **Born in PostgreSQL.** `assistant_turn`, `document_upload_session`, `esignature_webhook_journal`, `plan_intake`,
  and `sandbox_usage` have no write mirror: whenever `NAVIGATOR_POSTGRES_HOST` is set the application reads and writes
  them in PostgreSQL only. After the mirror groups, the `born_in_postgres` arm copies the rows SurrealDB held before
  that switch, keeping each row's identity, and never touches a row PostgreSQL already holds. Run it when a deployment
  first sets the host, before traffic writes a period PostgreSQL would then keep in place of SurrealDB's.
- **One at a time.** A run holds a PostgreSQL advisory lock while it writes; a second invocation exits as busy.

Exit status is 0 when the post-run parity report is clean, 1 when a plan has rows to write or a completed run has
failures or parity drift, 2 when a store is unavailable or the run could not advance, and 3 when a run paused at
`--max-batches`; a backfill is not a cross-store snapshot. The scoped protocol below requires writer quiescence.

Before relying on a backfill report, run `ops postgres mirror parity` and require a clean result.

### Scoped family enrollment

`satellites:1` enrolls exactly `attestation`, `authority_use`, `citation`, `contract_review`, `efiling_transactions`,
`expunge_record`, `expunge_request`, `git_access_token`, `matter_caption`, `playbook`, `project_module`,
`social_oauth_attempt`, and `testimonial`. `notation:1` enrolls exactly `notation`, `answer`, `notation_event`,
`notation_clause`, `notation_source`, `asset`, `notation_document`, and `notation_document_version`. Both scopes are
disjoint from global completion. The additional scoped sets are:

- `identity:1`: `entity`, `person`, `person_mailbox`, `address`, `credential`, `email_token`, `person_delegate`,
  `person_external_identity`, and `person_phone`.
- `tenancy:1`: `firm`, `brand`, `firm_brand`, `person_firm_role`, `project`, and `person_project_role`.
- `application:1`: `template`, `mailroom`, `letter`, `lead`, `lead_consent`, `project_deadlines`, `project_service`,
  `summary_sender_approval`, `summary_sender_approval_event`, `inbox_triage`, `deployment_secret_change`, `matter_page`,
  `notation_document_comment`, `firm_integration_secret`, `sent_email`, and `journal_entry`.

Ordinary application admission requires completed global history and every registered scoped family. Signature and
Notarization retain their existing PostgreSQL authority. Identity imports preserve natural mailbox ownership; Tenancy
verifies native lifecycle history rather than replacing it. Application imports preserve immutable Template pins,
encrypted authenticated context, proven email retention exclusions, and complete Journal projections and history.
Retained native Journal and Sent Email rows remain authoritative; their typed containment proofs account for them.

Apply the checked-in PostgreSQL migrations through the existing operator migration command before enrollment. Use the
importer binary before launching application or worker binaries that require the new scope. PostgreSQL connections and
operator commands remain available while application readiness refuses missing, malformed, or ambiguous receipts. A
configured database is never permission to hide unimported history.

Startup rejects completed scopes outside its registered versions. Mutating global imports and reconciliation also reject
unsupported scopes or invalid completed receipts, while a fresh global import remains available before family
activation. Use operator and writer binaries that understand every activated scope; read-only reports and recovery
inspection remain available.

1. Stop every old application, worker, scheduler, and other writer, including Person merge operations. Maintain that
   freeze through final verification and activation; a PostgreSQL importer lease serializes importers, not old writers.
   A reused development environment is not evidence of quiescence.
2. Produce a strict operational export with the existing archive procedure and its operator-quiesced assertion. Obtain
   the matching export and receipt from that archive. The assertion records capture conditions; it does not stop
   writers.
3. Run the bounded import using that pair. It verifies the actual export bytes, then compares the live frozen export,
   schema, raw census, decoded rows, complete content, references, and target constraints. Source-only rows import in
   registered dependency order. Pending satellite differences refresh; Asset imports preserve existing PostgreSQL rows.
   Unexplained target-only identities and conflicting PostgreSQL Person references block completion and require review.
   The typed Asset union exception below records independently native ownership without deleting or overwriting rows.
4. Continue with the recorded run id until its final verified capture has no blocking differences. Activate explicitly
   while all writers remain frozen, deploy only the guarded writer version, and then resume traffic and workers. This is
   an operational sequence, not a cross-database transaction or permission for a mixed-version writer window.

```bash
hudson ops postgres backfill --family satellites:1 \
  --capture /path/to/capture.surql --capture-receipt /path/to/capture.receipt.json --max-batches 20 --json
hudson ops postgres backfill --family satellites:1 --resume <run-id> --max-batches 20 --json
hudson ops postgres backfill --family satellites:1 --resume <run-id> --activate --json
```

If the source changed after capture, reestablish the writer freeze and produce a fresh strict pair. Replace only a
pending capture with `--resume <run-id> --refresh-capture --capture ... --capture-receipt ...`. This retains the old
content-free receipt in history and invalidates prior verification while preserving applied progress. Repeating the
identical verified pair is idempotent. Activation makes both the capture and its enrollment immutable.

Completed scoped replay validates the exact persisted receipt using PostgreSQL alone; it needs neither source access nor
capture files. Reports identify the scope and sealed capture basis and retain historical orphan diagnostics. A later
source import, reconciliation, or first global import cannot overwrite or resurrect activated PostgreSQL rows.

#### Notation enrollment checks

Use the same capture and resume procedure with `--family notation:1`. All registered scopes must be active before
application or worker readiness succeeds. The Notation scope also checks referenced Template version pins against the
frozen capture. A pin difference blocks activation until the existing catalog copy is repaired and verified. Template
ownership remains unchanged, and native Notation creation refuses a missing PostgreSQL Template.

Asset parity compares the full metadata image after excluding the reserved verification marker from Source input. An
onboarding overlap retains PostgreSQL visibility and its update timestamp when all remaining fields and metadata match.
Retaining a native verification marker requires independent canonical PostgreSQL completion evidence for the envelope
and Project; a Notation-scoped key also requires the same Notation. A metadata marker alone establishes no ownership.
Proven native target-only onboarding rows remain in PostgreSQL; unknown provenance and every other unexplained column or
metadata difference block activation. Reports show raw Source and PostgreSQL counts alongside resolved native overlaps,
proven native target-only rows, and unknown target-only rows. These counts cannot cancel unrelated drift.

Reset verification separately reports retained native protection and refuses any missing captured protection. Documents
and immutable versions use canonical parent checks and transactional head comparison. Comment bodies remain in
SurrealDB; comment parent admission resolves canonical document/version rows, including PostgreSQL-only versions.
Project archive restoration holds verified parent leases until its Asset and document facts commit. Private historical
restore paths preserve verified missing-parent associations; ordinary new writes still require their parents, and
same-id conflicts are refused. This completion boundary does not make the whole Project restore one transaction.

For a fresh, task-owned synthetic development database, complete the same quiesced enrollment before starting a
configured application or worker. Ordinary environment provisioning and sample seeding do not bypass readiness or
activate a scope automatically. A legacy-only development process retains its existing bootstrap contract.

Retain completed scoped runs, checkpoints, receipt history, and failure evidence with the canonical PostgreSQL backup.
The existing Iceberg recovery drill restores these actual ledger tables, rejects missing or partial authority proof, and
verifies readiness after the complete proof is restored. Losing receipt metadata must fail closed; recreating a
completed receipt from source history would risk replaying deleted native rows.

`--verify-assets` also checks retained Asset bytes and public/private object-key references from both stores. Each
object read or metadata check has a 120-second deadline. A timeout counts as unreadable and fails verification, while
other objects are still checked. Run byte verification in an approved private environment; reports contain aggregate
counts without object keys or bytes. These live checks complement a verified recovery backup and a coordinated final
writer cutoff.

## Shadow reads

A [Shadow Read](glossary/shadow-read.md) compares two query implementations while one answers the caller.
Navigator application reads use PostgreSQL directly. They do not sample a second store or contact SurrealDB for
runtime comparisons. Missing authority history fails closed before the read.

Explicit captured imports verify table images, references, immutable pins, and protected native history through
`hudson ops postgres mirror`. Those operator checks use the captured Source and PostgreSQL parity adapters,
separate from ordinary application requests.
