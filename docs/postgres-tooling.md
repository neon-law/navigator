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

Run it as a Job, with a migration role's credentials, and start services only after it succeeds. `navigator journal
migrate` calls the same runner. Every migration lets SQLx own its transaction, lock and `_sqlx_migrations` row: no file
contains `BEGIN`/`COMMIT` or writes a ledger of its own.

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
2. Run `navigator ops postgres mirror reconcile` against the saved source versions and resolve remaining drift.
3. Run the complete migration command.

A missing Template version stops the constraint migration transactionally: it leaves every row and all three constraints
unchanged. Repair the source-backed mirror and retry; never invent a version or delete a pin to make the migration pass.

## PostgreSQL mirror tables and reconciliation

SurrealDB is the source of truth for every read and every write's outcome. The following tables also have a recoverable
PostgreSQL mirror in [`store::postgres_mirror`](../store/src/postgres_mirror.rs):

- **Identity:** `person`, `entity`, and the `entity_type` and `jurisdiction` rows an Entity names. The Person and
  Entity writers in `store::persons` and `store::entities` call the mirror. Mirroring an Entity first mirrors each
  reference row PostgreSQL does not hold.
- **Tenancy:** `firm`, `brand`, `firm_brand`, `project`, `person_firm_role`, and `person_project_role`. The Firm,
  Brand, and Project commands in `store::firms`, `store::brands`, and `store::projects` call the mirror.
- **Documents:** `asset`, both document assets and bare content assets. Every asset writer in `store::assets` and
  `store::documents` calls the mirror, including each revision a soft deletion, restore, Legal Hold, or purge stamps.
- **Workflow:** `notation`, `notation_event`, `answer`, and `notation_clause`. `store::notations`,
  `store::notation_events`, `store::answers`, `store::notation_reset`, and `store::notation_clauses` call the mirror.
- **Templates:** versioned Template metadata. `store::templates::save_version` mirrors each newly written version and
  the retired version; the nullable unique `current_key` is copied from SurrealDB, while bodies remain in Assets.
- **Editorial:** `post`, `social_handle`, and `social_publication`.
- **Social Accounts:** `social_account` and `social_account_selection`. Connect, refresh, and revoke mirror the
  encrypted KMS bundle and account metadata after each successful SurrealDB write. The account mirror preserves
  ciphertext and wrapped-key bytes, and never decrypts them. Firm, Brand, and connected or revoking Person links remain
  explicit. The saved-selection mirror preserves the chosen Brand/provider destination without credentials or publishing
  authority; disconnecting the account leaves that selection intact. The one-use Social OAuth Attempt, including its
  PKCE verifier, stays in SurrealDB.
- **Catalog:** `authority`, `glossary_term`, `question`, `notation_package`, `notation_package_template`, and
  `notation_rate`. These are shared catalogs owned by no Firm or Project. The Authority, glossary, Question, and
  Notation package writers in `store::authorities`, `store::glossary`, `store::questions`, and
  `store::notation_packages` call the mirror; the canonical seed reaches it through those writers. Notation rates have
  no SurrealDB writer and reach PostgreSQL only through `reconcile`.
- **Identity detail:** what a Person or Entity holds beside its own row: `address`, `credential`, `email_token`,
  `entity_role`, `person_delegate`, `person_external_identity`, and `person_mailbox`. Their writers in
  `store::addresses`, `store::credentials`, `store::email_tokens`, `store::entity_roles`, `store::delegations`,
  `store::external_identities`, and `store::persons` (mailbox claims) call the mirror.
- **Communications:** `summary_sender_approval` and its append-only
  `summary_sender_approval_event`. The approval and audit writers call the mirror after their SurrealDB writes.
- **Inbox:** `inbox_conversation`, `message`, `inbox_ingress`, `inbox_assignment`, and
  `inbox_delivery_attempt`. Intake, assignment, reply, delivery, and alert writers mirror their successful SurrealDB
  writes. Parity compares the full transcript and audit rows. Reconciliation restores missing parents before children
  and repairs mutable rows; ingress, assignment, and delivery-attempt evidence remains insert-only, with any changed or
  extra target row reported as drift. Firm and Project references remain explicit, and internal notes keep their
  recorded direction and visibility.
- **Inbound email delivery:** `email_receipt`, `email_delivery`, and `email_delivery_attempt`. The receipt and Slack
  delivery writers mirror each successful write. Reconciliation restores parents before children; receipt deletion
  cascades through the delivery and attempt records.
- **Outbound email audit:** `sent_email`. Its writer in `store::sent_emails` mirrors each recorded attempt.
  Reconciliation inserts missing rows; changed or extra audit rows remain drift for review. No audit history is
  overwritten or deleted. The optional Project foreign key preserves matter scope.
- **Deadlines:** `project_deadlines`. Its writer in `store::project_deadlines` calls the mirror after a deadline write.
- **Services:** `project_service`. Its writer in `store::project_service` mirrors a Service after recording it and
  removes the target row when a Project cascade removes the source. Each row keeps its Project and served Document; the
  Document must belong to that same Project. Optional recipient, declarant, and sent-email references are mirrored when
  absent from PostgreSQL. The Service's named recipient, method, address, declarant, and service date are copied as
  recorded; this mirror does not derive a deadline. Letter references remain UUIDs until the Letter table is mirrored.
- **Project references:** `project_reference`. The writer in `store::project_references` mirrors a verified Person or
  Entity reference after its SurrealDB write. This row does not create Project participation or grant access; the portal
  party list does not render it. PostgreSQL enforces exactly one Person or Entity endpoint and the source reason set.
- **Project records:** `project_module`, `statutory_deadline`, `testimonial`, and `playbook`, mirrored whole by
  `store::postgres_mirror::records`. Their writers in `store::project_modules`, `store::statutory_deadlines`,
  `store::testimonials`, and `store::playbooks` mirror each successful write. Each row keeps its record UUID and names
  its Project, Person, or Entity by foreign key; the parent is mirrored first when PostgreSQL lacks it.
- **Notation inputs:** `notation_source` and `notation_draft`, mirrored whole by the same module.
  `store::notation_sources` and `store::notation_drafts` mirror each write, and deleting a preview draft removes its
  row. A source names its Communication by foreign key, never a copy of the message; deleting a Notation in PostgreSQL
  removes its sources.
- **Git access:** `git_repository` and `git_access_token`, mirrored whole by the same module. `store::git_repositories`
  and `store::git_access_tokens` mirror each create and revocation. A token is stored only as its SHA-256 hash, never
  its plaintext; an unscoped token has no Project.

After a Surreal write to a mutable mirrored row succeeds, the writer reads it back from SurrealDB by key and upserts it
into PostgreSQL. If SurrealDB no longer has that row, the writer deletes it from PostgreSQL. Append-only audit events
use insert-only semantics and are never overwritten or deleted by the mirror.

| Setting | Effect |
| --- | --- |
| `NAVIGATOR_POSTGRES_MIRROR=1` and `NAVIGATOR_POSTGRES_HOST` set | Mirror each write through the shared pool |
| Either missing | Do nothing; PostgreSQL is never contacted |

Each row has one identity in both stores. A Firm, Brand, Project, Project participation, asset, Notation, Notation
Event, answer, and Post use their record UUID. A `firm_brand` row uses its Brand key. A Firm membership uses its Person
and Firm. A Social Handle uses its Brand and provider. A Social Publication uses its Post and provider. Both schemas
enforce these keys as unique. An upsert writes only when a column differs, so a replay changes nothing. Timestamps
compare at PostgreSQL's microsecond precision. When a Brand and slug in PostgreSQL carry a different Post id,
SurrealDB's row replaces the stale Post.

A mirror waits synchronously for up to three projection attempts within a total fifteen-second budget. Each attempt has
a four-second timeout and reads the current source again. Transient connection, timeout, serialization, and lock
failures retry; invalid timestamps and integrity constraints stop immediately. Retries never replay the domain write or
an external effect. A failed projection logs its table and sanitized error at error level and leaves the committed
Surreal write intact. Returning a retryable request error after that commit could duplicate external effects.

Deployment opt-in requires both `NAVIGATOR_POSTGRES_ENABLED = "true"` and `NAVIGATOR_POSTGRES_MIRROR = "1"` in the
private deployment's `config.toml`. Ship requires a clean initial reconciliation before applying the application and
installs a reconciliation CronJob every five minutes. It uses the existing CLI image, verify-full TLS, and the app
login. Concurrent scheduled runs are forbidden; failed or overdue recovery raises a critical monitoring rule. Alert
routing and kube-state metrics must be operational in the deployment. Disabling the flag stops web projections;
explicitly remove the recovery CronJob and its monitoring Rule when retiring recovery.

Durable source rows and target drift allow the recurring scan to rediscover missing, differing, and extra mutable rows
after an application process exits. This is eventual convergence when both stores recover and writes settle, not a
transaction across databases, a durable event queue, or preservation of intermediate versions. A source row created and
deleted before any successful projection cannot be reconstructed. Only tables with registered mirrors participate; SQL
schema coverage is incomplete. Do not use this policy when every intermediate event must be retained. The initial scan
is a release gate, not proof that concurrent writes remain equal indefinitely. Each group's `parity` compares its rows
and names every divergent key; `reconcile` retries those rows from SurrealDB and returns the comparison after repair.
PostgreSQL foreign keys refuse rows whose required parent is absent, leaving them divergent until the parent exists
there. Append-only event rows missing from SurrealDB remain in PostgreSQL and are reported.

`postgres_mirror::tenancy::parity` and `tenancy::reconcile` do the same for the tenancy tables, parents first; run them
before the editorial pair. Before a Firm, Project, or membership is upserted, the Person or Entity it names is mirrored
when PostgreSQL does not hold it. A reference SurrealDB cannot supply is refused by its foreign key and stays divergent.
`postgres_mirror::social_accounts::parity` and `social_accounts::reconcile` compare and repair encrypted Social Account
connections after the identity and tenancy mirrors. Before an account is upserted, its Firm, Firm-owned Brand, and
connected or revoking Person are mirrored. Ciphertext and wrapped data keys are copied as stored; this mirror never
decrypts them. The stable source key, KMS context, key version, status, and timestamps remain part of parity. The saved
Social Account Selection is mirrored separately by `social_account_selections`; it preserves the Brand/provider record
key and selected destination without credentials or publishing authority. A cancelled or disconnected Social Account
does not remove that saved selection. The one-use OAuth Attempt and PKCE verifier remain in SurrealDB.
`postgres_mirror::identity::parity` and `identity::reconcile` compare and repair every identity row; run them after the
tenancy pair. They name rows by key only. A Person whose email or sign-in subject PostgreSQL still holds under another
id is refused, not overwritten, and converges once that other row is mirrored. Surreal timestamps are RFC 3339 text; a
row whose timestamp does not parse is refused and reported, never given a substitute time.

`postgres_mirror::documents::parity` and `documents::reconcile` compare and repair every asset row; run them after the
tenancy pair. An asset row copies its checksum, storage keys, revision identity (`project_id`, `slug`, ordered by id),
visibility, mail provenance, and its soft-deletion, Legal Hold, and purge stamps as stored; its JSON columns are copied
without being re-parsed. Before an asset is upserted, its Project and the Person who deleted it are mirrored when
PostgreSQL does not hold them. Reports name assets by id only, never by filename or storage key.

`postgres_mirror::catalog::parity` and `catalog::reconcile` compare and repair the catalog tables, a Notation package
before its templates; run them after the editorial pair. Mirroring a package template first mirrors its package when
PostgreSQL does not hold it, and mirrors the exact Template version on demand. An Authority naming an archived Asset
PostgreSQL lacks is refused by its foreign key and stays divergent until that Asset exists there. Template foreign keys
retain pinned package versions. A citation, slug, or code PostgreSQL holds under another id is refused, not overwritten;
`reconcile` deletes the stale row and converges.

`postgres_mirror::workflow::parity` and `workflow::reconcile` compare and repair every Notation, Notation Event, and
answer; run them after the catalog pair. Notation Event ids are UUIDv7, so `ORDER BY id` gives the same latest event per
`(notation_id, machine_kind)` in both stores. Answers sort by insertion time at PostgreSQL's microsecond precision, then
UUID for deterministic ties, so latest-answer reads agree across both stores. Events and answers are append-only: a
correction is another row, and an answer keeps its respondent and author apart. A Notation SurrealDB no longer has takes
its PostgreSQL events and answers with it; one SurrealDB still holds keeps the Notation in place, and parity reports
both. Before a Notation or answer is upserted, the Notation Package or Question it names is mirrored when PostgreSQL
does not hold it. Each Notation and Notation Event also mirrors its exact Template version on demand; foreign keys
retain those versions even after a newer version becomes current.

`postgres_mirror::notation_clauses::parity` and `notation_clauses::reconcile` compare and repair the custom clauses
attached to each Notation; run them after workflow. Each row keeps its author, body, and render position. Body edits,
deletions, and transactional position swaps mirror after the SurrealDB write succeeds. PostgreSQL deletes its child rows
when a Notation is deleted.

`postgres_mirror::identity_detail::parity` and `identity_detail::reconcile` cover the identity-detail tables; run them
after the identity pair. Rows with a record UUID keep it. An `entity_role` row is keyed by its Person, Entity, and role,
the edge's unique tie in SurrealDB. A `person_mailbox` row's key is the claimed mailbox, a lowercased email, so the
mirror keys claims by their Person and copies all of one Person's claims together; a report never names a mailbox.
Before a row is upserted, the Person, Entity, or Jurisdiction it names is mirrored when PostgreSQL does not hold it. No
foreign key cascades: SurrealDB does not cascade a Person delete, so an orphaned child is reported, not removed. An
`email_token` row carries the SHA-256 hex of its token, the only form SurrealDB stores; the plaintext exists only in the
emailed link.

`postgres_mirror::communications::parity` and `communications::reconcile` compare and repair the approval and audit
tables; run them after the identity and tenancy pairs. An approval is identified by its record id and carries its Firm,
optional Project, provider account, sender email, Person, status, and update time. Its Firm, Project, and Person parents
are mirrored when PostgreSQL does not hold them. Audit events also carry the actor Person and are insert-only: replay
with the same id does not overwrite an existing event, and an event missing from SurrealDB is not deleted from
PostgreSQL. A missing parent or a failed write remains divergent and is reported by parity. Times are compared at
PostgreSQL microsecond precision.

`postgres_mirror::deadlines::parity` and `deadlines::reconcile` compare and repair Project deadlines; run them after the
tenancy, document, and identity pairs. A deadline keeps its Project, due time, authority and source, optional source
Asset and owner Person, status, calendar role, reminder lead days, replay key, and insertion and update times. The
Project, Asset, and Person parents are mirrored when PostgreSQL does not hold them. Invalid RFC 3339 timestamps are
refused and reported; valid times compare at PostgreSQL microsecond precision. Reconcile repairs divergent rows and
removes target-only deadlines when the source row is gone.

`postgres_mirror::project_services::parity` and `project_services::reconcile` compare and repair recorded Service rows;
run them after the tenancy, document, identity, and communications groups. The Project and served Document must agree on
Project ownership; a mismatched Document is refused. Optional recipient Person, recipient Entity, declarant Person, and
sent-email parents are mirrored on demand. The nullable Letter id remains source data because PostgreSQL does not yet
have a Letter table. Reconcile removes target-only Service rows when SurrealDB no longer holds them.

`postgres_mirror::project_references::parity` and `project_references::reconcile` compare and repair verified Project
Reference rows; run them after identity and tenancy. A reference is keyed by Project, party kind and id, and reason. Its
Person or Entity endpoint and Project parents are mirrored when PostgreSQL does not hold them. Reconcile removes a
target-only row when SurrealDB no longer holds the reference. A Project Reference does not substitute for
`person_project_role` and is not an authorization input.

## Mirror backfill

A [Mirror Backfill](glossary/mirror-backfill.md) copies the rows SurrealDB holds and PostgreSQL lacks: history written
before a deployment turned the mirror on, or a write whose projection failed and was never repaired. It writes each row
through the same per-row mirror the write hook and `reconcile` use, so a backfilled row is the row the mirror would have
written. It never overwrites a differing row or deletes a PostgreSQL-only row; those stay with `ops postgres mirror
reconcile`.

```bash
navigator ops postgres backfill --dry-run --json          # the plan; writes nothing
navigator ops postgres backfill --dry-run --verify-assets --json  # include referenced objects
navigator ops postgres backfill --batch-size 500 --json   # start a run
navigator ops postgres backfill --max-batches 20 --json   # start a run, stop after 20 batches
navigator ops postgres backfill --resume <run-id> --json  # continue a run from its checkpoints
```

- **Plan.** `--dry-run` compares every mirror in registry order and lists, per table, the rows a run would write (by
  UUID), the batches they need, and the differing and PostgreSQL-only rows it would leave. It writes nothing, not even a
  run record. The same stores give the same plan.
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
- **One at a time.** A run holds a PostgreSQL advisory lock while it writes; a second invocation exits as busy.

Exit status is 0 when the post-run parity report is clean, 1 when a plan has rows to write or a completed run has
failures or parity drift, 2 when a store is unavailable or the run could not advance, and 3 when a run paused at
`--max-batches`; a backfill is not a cross-store snapshot. Writes that land during a run reach PostgreSQL through the
write mirror and recurring reconciliation.

Before relying on a backfill report, run `ops postgres mirror parity` and require a clean result.

`--verify-assets` also checks retained Asset bytes and public/private object-key references from both stores. Each
object read or metadata check has a 120-second deadline. A timeout counts as unreadable and fails verification, while
other objects are still checked. Run byte verification in an approved private environment; reports contain aggregate
counts without object keys or bytes. These live checks complement a verified recovery backup and a coordinated final
writer cutoff.

## Shadow reads

A [Shadow Read](glossary/shadow-read.md) proves a PostgreSQL read returns what the SurrealDB read it will replace
returns, before any caller depends on it. SurrealDB still answers every read.
[`store::postgres_shadow`](../store/src/postgres_shadow.rs) runs the equivalent PostgreSQL query in a detached task with
a two-second budget, and compares both rows through the mirror's own image, so timestamps compare at microsecond
precision exactly as `ops postgres mirror parity` compares them. The caller's result, latency, and errors never depend
on PostgreSQL.

| Store | Shadowed reads |
| --- | --- |
| `entities` | `entities::find_by_id` |
| `firms` | `firms::find_by_id` |
| `notations` | `notations::find_by_id` |
| `projects` | `projects::find_by_id`, `projects::find_by_code` |

| Setting | Effect |
| --- | --- |
| `NAVIGATOR_POSTGRES_SHADOW_READS` unset or empty | Off; PostgreSQL is never contacted for a read |
| `NAVIGATOR_POSTGRES_SHADOW_READS=projects,firms` | Shadow those stores' reads (any of the four names above) |
| `NAVIGATOR_POSTGRES_SHADOW_SAMPLE` | Share of covered reads shadowed, `(0, 1]`; default `0.1` |

Shadow reads also need `NAVIGATOR_POSTGRES_HOST` and the mirror's PostgreSQL login. An unknown store name or a sample
outside `(0, 1]` turns every shadow read off and logs one error naming the setting; a read never fails on it.

Each verdict is logged under the `navigator::postgres_shadow` target with the table, the row's UUID, and `match`,
`mismatch` (a row missing from one store, or a column that differs), or `unavailable` (PostgreSQL errored or exceeded
the budget). Mismatches log at warning level, matches at debug. No column value is ever logged.
`postgres_shadow::counts()` holds the same tallies per table for the running process.

A store's reads may cut over to PostgreSQL in a deployment only when, with the mirror enabled and its recurring
reconciliation clean throughout:

1. Its shadow reads ran at sample `1` for at least seven consecutive days.
2. Each covered table recorded at least 1,000 compared reads in that window, or every read it served if fewer.
3. Mismatches are zero. Any mismatch restarts the window once its cause is fixed and parity is clean again.
4. `unavailable` verdicts are at most 0.1% of compared reads.

These thresholds are evidence for one store's read path; they do not cover a store that is not shadowed.
