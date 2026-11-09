# Iceberg archive — design

Status: **writer landed and wired for both lanes** (`archives::iceberg::author_snapshot`) — the scheduled `Archives`
workflow's `iceberg_telemetry` phase (`archives/src/workflow.rs`) promotes every table declared in Navigator's SurrealDB
schema (the `application/` lane) alongside `otel_logs`, `otel_traces`, and `otel_metrics` (the `telemetry/` lane) to
Iceberg tables every run. BigLake registration is the remaining step. This doc names every table, its partitioning, the
GCS layout, the BigQuery wiring, and the retention policy.

The current archive CronJobs fire at **05:55 and 17:55 UTC** each day. The application lane writes one full-table file
under `application/<table>/data/dt=<date>/run=<HHMM>/snapshot.parquet` for each scheduled time. Promotion selects that
run prefix, so its current Iceberg snapshot includes one extraction rather than both daily full-table copies. Telemetry
still promotes the day's collector files. Both phases use the dedicated archive storage handle. The Slack bot posts the
scheduled time and outcomes to the deployment's messages channel; it reports promotion failures too.

## Evaluation outcome (recorded 2026-06-14)

The writer uses the **`iceberg` crate (0.10)** for format correctness — a subtly-wrong hand-rolled Avro manifest is a
silently unreadable archive, and this lake holds binding legal records. Two frictions shaped the integration:

- **Arrow major mismatch.** `iceberg` 0.10 targets **arrow 58**; the workspace is on **arrow 60**. We never hand the
  crate an arrow value — `arrow_schema_to_iceberg` derives the Iceberg schema from the arrow-60 field names/types
  directly, so the two arrow majors never meet at a type boundary.
- **Storage rule.** Bytes must flow through `cloud::StorageService`, never a GCS SDK (AGENTS.md). `iceberg`'s manifest
  writers only write to their own `FileIO`, so the writer points them at an **in-memory** `FileIO`, passing the final
  `gs://` object URIs as the output paths (the memory backend treats them as opaque keys, so the paths embedded in the
  manifest list and snapshot are the real ones), then reads the bytes back and returns them for the caller to persist.

**v1 simplifications** (documented so a reader isn't surprised): the table is **unpartitioned** — point-in-time queries
come from the snapshot log (each run appends a snapshot), which is the design's stated mechanism; `dt=` identity
partitioning and per-file column bounds are deferred refinements. The append-only snapshot log is preserved across runs
by chaining from the prior `v<N>.metadata.json` (`TableMetadataBuilder::new_from_metadata`).

## Why

The scheduled [`Archives`](durable-workflows.md) Restate workflow snapshots every table declared in Navigator's
SurrealDB schema to **Parquet** on GCS (`archives/src/snapshot.rs`, `archives/src/parquet_io.rs`). The objects live
under a table and date prefix in the dedicated archive bucket. Application snapshots additionally select their scheduled
run; collector telemetry keeps its existing date prefix:

```text
gs://<archive-bucket>/application/<table>/data/dt=<yyyy-mm-dd>/run=<HHMM>/snapshot.parquet
gs://<archive-bucket>/telemetry/<table>/data/dt=<yyyy-mm-dd>/part-<uuid_v7>.parquet
```

Each run promotes its Parquet into **Apache Iceberg table format**: metadata JSON, manifest lists and manifests. Readers
select an Iceberg snapshot instead of combining every Parquet file in a date prefix. The current application snapshot
refers only to its full extraction; the snapshot log preserves earlier extraction points.

## What "promote to Iceberg" means concretely

For each table prefix we additionally write, every scheduled run:

- `metadata/v<N>.metadata.json` — the table metadata: schema (with column ids), partition spec, current snapshot id,
  and the snapshot log. Version `N` increments each run; `metadata/version-hint.text` points at the current `N`.
- `metadata/snap-<snapshot-id>-<uuid>.avro` — the **manifest list** for that snapshot (one row per manifest).
- `metadata/<uuid>-m0.avro` — the **manifest file** listing the data files added that run, with per-file row counts and
  column bounds.
- Application data: `data/dt=<date>/run=<HHMM>/snapshot.parquet`.
- Telemetry data: `data/dt=<date>/part-<uuid_v7>.parquet`.

The persistent canonical PostgreSQL lineage lives under `postgres/navigator/<table>/`. Its generation data files use
`data/dt=<date>/run=<scheduled-slot-or-extraction-time>/snapshot-<sha256>.parquet`; content-addressed names keep a
same-slot re-export from replacing a file that an older Iceberg snapshot still references. PostgreSQL schema evolution
preserves field IDs for the same live database relation and source attributes, including rename, add, drop, and column
reordering. Added attributes get IDs above the previous maximum, including list elements. Changed relation identity,
missing or malformed source mapping, and incompatible mapped type changes fail closed. Relation identity includes the
database name and database, relation, and storage identifiers. This does not promise continuity across dump/restore or
OID reuse. The separate `postgres/_paired/<generation>/` exports begin a fresh lineage per generation, and application,
telemetry, and other sources retain their existing schema-evolution behavior.

The data files we already produce are reused as-is; the new bytes are metadata. The snapshot is **append-only**: each
run adds a new snapshot pointing at that run's data files, and prior snapshots stay valid — so the lake mirrors the
append-only [per-Project git repos](project-repositories.md) and never rewrites history.

## Tables

### SurrealDB table snapshots (full-table, twice daily) — `application/` lane

`archives::tables::ALL_TABLES` is generated from every `DEFINE TABLE` in the shipped `store/src/schema/navigator.surql`.
Adding a table to that schema adds it to the analytical lake in the same build; a read failure remains visible in the
scheduled summary rather than becoming an unregistered-table gap.

Each run writes a **full snapshot** of the current SurrealDB table, then the `iceberg_telemetry` workflow phase promotes
it to an Iceberg table under the `application/` lane, reusing the same `author_snapshot` / `author_iceberg_for_prefix`
writer described above. Record links land as stable `table:key` strings and datetimes as RFC 3339 strings, because the
Parquet contract is nullable UTF-8 columns. Point-in-time queries come from Iceberg's snapshot log, not from diffing.
These are binding records: the snapshot log is never pruned (`expire_before_ms: None`), unlike the telemetry lane below.

### PostgreSQL coverage generations

The `snapshot` step requires reviewed Navigator and Forgejo inventories when PostgreSQL is configured. Supply
`NAVIGATOR_ARCHIVE_COVERAGE` with both manifests and the separate `NAVIGATOR_ARCHIVE_FORGEJO_USER` and
`NAVIGATOR_ARCHIVE_FORGEJO_PASSWORD` credentials. `NAVIGATOR_ARCHIVE_POSTGRES_USER` and
`NAVIGATOR_ARCHIVE_POSTGRES_PASSWORD` set the Navigator login; unset, it reuses the `NAVIGATOR_POSTGRES_*` login. The
deployed shape is under [Where it runs](#where-it-runs). Forgejo uses database `forgejo` on the same server with the
same TLS settings. Missing configuration, denied reads, or source failures record a PostgreSQL failure in the digest and
terminate the invocation. Completed SurrealDB snapshots remain available. Recovery requires a new invocation because
replay retains the journaled extraction outcome.

The same rule holds for every table with rows: a table that fails to write fails the run. The digest is posted first,
opening with `Iceberg data export FAILED`, the failure count, and one line per failing table with the first line of its
error; then the run ends with a terminal error naming those tables, so Restate and DBOS both record it as failed. A
table that is empty is skipped before the bucket is touched and does not fail the run.

1. **Verify coverage.** Read `pg_catalog` so denied privileges cannot hide relations. Check every application schema,
   DBOS table, AGE catalog and graph relation, migration ledger, sequence, materialized view, and large-object table
   against the reviewed manifest. Added, dropped, inaccessible, unclassified, and unsupported relations fail the run. An
   ordinary view needs an explicit reconstruction reason; stored data cannot use that exclusion.
2. **Extract each database.** Read tables in a `REPEATABLE READ, READ ONLY` transaction through server-side cursors,
   8192 rows per fetch and Parquet row group, with a 300-second statement timeout. Read inherited tables with `ONLY` so
   each physical row appears once. Record each database's MVCC watermark, cutoff, server/extension versions, column
   types and schema digest. List sequence relations separately because sequence state is outside MVCC.
3. **Publish the pair.** Extract both sources before uploading. Write valid zero-row files for empty tables, then
   author Iceberg metadata through the existing writer. Publish `postgres/_paired/<generation>.json` last. Readers
   require that marker and verify every Parquet SHA-256. Failed writes can leave uncommitted objects and metadata.

UUID, timestamps, dates, booleans, integers, floats, byte arrays, and text arrays retain their supported Iceberg types.
JSONB retains PostgreSQL's text with the Parquet JSON annotation. Numeric values use exact text where PostgreSQL exceeds
Iceberg decimal precision. AGE values and catalog identifiers retain source text and PostgreSQL type declarations. Any
unsupported column type fails the generation.

A pair records `cross_database_atomic: false`. A coordinated write-quiescence or recovery-point procedure must be proved
before claiming a shared transactional recovery point. Rehydration applies each database's own schema procedure into an
isolated target; the synthetic drill reads Iceberg, restores source column types, and compares rows and hashes. AGE
schema reconstruction resolves target catalog OIDs through native procedures. This drill does not certify full Forgejo
service boot, DBOS replay, or external-byte recovery. Git, LFS, attachments, and document bytes have separate backup
contracts. Physical PostgreSQL backup and WAL remain the point-in-time recovery path.

The bounded `archives::postgres::export_generation` helper exports a named single-source table set. Its manifest also
represents journaled single-source workflow results. Configured runs use `archives::coverage::export_pair`. See [Iceberg
Archive](glossary/iceberg-archive.md) and [archive configuration](../archives/README.md).

### Telemetry table snapshots (promoted, scheduled) — `telemetry/` lane

`otel_logs`, `otel_traces`, and `otel_metrics` (`archives/src/workflow.rs`'s `TELEMETRY_TABLES`) are promoted to Iceberg
tables each run by the same `iceberg_telemetry` phase, under the `telemetry/` lane. `otel_logs` keeps its full snapshot
log; `otel_traces` and `otel_metrics` prune snapshots older than 30 days to match their GCS lifecycle window.

## Partitioning

The Iceberg partition spec is **unpartitioned**. The directory labels `dt` and `run` isolate files for authorship; they
do not declare an Iceberg identity transform. Snapshot selection provides point-in-time reads. Identity date
partitioning and column bounds remain separate refinements.

## GCS layout

One dedicated bucket, `NAVIGATOR_ARCHIVES_BUCKET`, opened through `cloud::archives_from_env`. Per table, under its lane
(`application/` for a SurrealDB entity table, `telemetry/` for an OTel table):

```text
gs://<archive-bucket>/application/<table>/
  metadata/
    version-hint.text                   # unlocked lane only — never written on the locked lane
    v<N>.metadata.json
    snap-<snapshot-id>-<uuid>.avro      # manifest list
    <uuid>-m0.avro                       # manifest file(s)
  data/
    dt=2026-06-10/run=0555/snapshot.parquet
    dt=2026-06-10/run=1755/snapshot.parquet
    dt=2026-06-11/run=0555/snapshot.parquet
```

`telemetry/<table>/` mirrors the same shape. Paired PostgreSQL files live under
`postgres/<database>/_paired/<generation>/<hex-schema>_<hex-table>/data/dt=<date>/snapshot.parquet`, with Iceberg
metadata beside each table's data. The completion marker is `postgres/_paired/<generation>.json`.

Bytes stay in `cloud::StorageService` (GCS in prod, `FsStorage` in dev) — the writer goes through the trait, never the
GCS SDK directly, per [AGENTS.md](../AGENTS.md).

### Offsite archive project

When the archive bucket lives in a GCP project other than the deployment's, set `NAVIGATOR_ARCHIVES_GCP_PROJECT_ID` on
the workflows-service pod to the archive project's id or numeric project number. Lane resolution
(`cloud::archives_from_env`) then verifies the bucket's owning project through the storage credential and **refuses to
write** when the match cannot be confirmed: a numeric value verifies by direct equality against the project number GCS
reports for the bucket; an id verifies that the bucket is outside the deployment project (a storage-scoped credential
cannot resolve ids to numbers). The Slack digest annotates the archive project and pins its console links with
`?project=`. Leave the variable unset for an in-project bucket and on non-GCS backends (staging SeaweedFS keeps its
existing variable set).

**What the variable accepts.** Either the archive project's id (e.g. `example-archive-project`) or its numeric project
number (e.g. `123456789012`). The two forms prove different things, because the workflows-service identity holds only
`roles/storage.objectCreator` and `roles/storage.objectViewer` on the bucket — no Cloud Resource Manager access — so the
only project identity GCS reports to it is the bucket's numeric project number:

- **A number** verifies by direct equality against that probe. This is the stronger form.
- **An id** cannot be resolved to a number under a storage-only credential, so the check verifies the property it
  can: the bucket's project differs from the deployment project's number, read from the GKE metadata server (no IAM
  required). The id is still carried into the Slack digest annotation.

**Fail-closed refusal.** The verification runs at the single choke point every archives-lane caller resolves its handle
through (`cloud::archives_from_env`), before any object write. Every unverifiable shape is an error, never a pass: a
probe error, a numeric mismatch, an id-form value whose bucket lands in the deployment project, or an id-form value with
an unreachable metadata server all refuse the lane, and the scheduled run records the failure. An unset or empty value
leaves the seam off.

**What the Slack digest annotates.** When the variable is set, the digest gains an `• *archive project* — <value>
(offsite bucket)` line, and every per-table console link is pinned with `?project=<value>` so the GCS browser opens the
bucket in the archive project's context rather than whichever project the viewer last visited.

## Immutability on the retention-locked lane

The offsite bucket carries a **locked** GCS Bucket Lock retention policy — 25 years (25 × 365.25 days, expressed in
seconds as `OFFSITE_ARCHIVES_RETENTION_SECONDS`), set at bucket creation and locked once verified. A locked policy
rejects both replacement and deletion of any object, so on that lane the writer's contract is **create-only**. The Git
bundle archive is the other locked lane (`<deployment>-bundles`, 7 years, see [archive
buckets](environments.md#archive-buckets)); it is a separate bucket because this unlocked in-project lane rewrites
`version-hint.text` every run.

- **Probe before write.** Every writer probes the bucket's policy (`retention_locked`) and branches on the answer. A
  backend that cannot determine its bucket's policy fails the run — an unprobeable locked bucket must never receive a
  destructive overwrite. The filesystem and S3 backends answer "no policy" (they have no bucket-policy surface).
- **Attempt-unique keys.** On a locked lane every write goes through `put_immutable` (create only when no live
  object holds the key). Nothing overwrites committed bytes: a scheduled application snapshot claims its `run=<HHMM>`
  generation key only when that generation is not yet taken; a re-extraction of the same slot or an unscheduled run
  lands under its own attempt-unique key (an ordinal for fingerprint companions, a uuid partition for re-extractions).
  The unlocked lane (dev, KIND, unlocked in-project buckets) keeps the documented within-generation overwrite on fixed
  keys.
- **No `version-hint.text`.** The hint is mutable by definition — a locked policy rejects replacing it — so the
  locked lane never writes it. Readers resolve the latest table version by taking the highest `v<N>.metadata.json` in
  the `metadata/` listing, the same log-chaining order the writer walks (`archives/src/iceberg.rs`).
- **Retention is the lock.** The `application/` lane carries no lifecycle rule; the locked policy is what prevents
  deletion for 25 years. Deleting archive data is neither a permission the writer identity holds nor a call the code
  makes.

## Catalog + BigQuery wiring — **operator decision required**

Two ways to make BigQuery read the lake; pick one before building the writer:

1. **BigLake / BigQuery managed Iceberg tables (recommended).** Register each Iceberg table once via a BigLake
   connection; BigQuery reads the Iceberg metadata directly, so schema evolution and snapshot adds show up without a
   per-run DDL. This is the closest to "it's just a table." Cost: a one-time BigLake connection + IAM on the bucket.
2. **BigQuery external tables over the Parquet (status quo, simplest).** A per-table `CREATE EXTERNAL TABLE` with
   `OPTIONS(format = 'PARQUET', uris = ['gs://…/application/<table>/data/*'])` — the pattern already used for the
   email-events stream (see [the email-events pipeline](email-events-pipeline.md)). No Iceberg metadata needed, but no
   time-travel and no schema evolution: it globs the data files. Use this only if BigLake is unavailable.

**Recommendation:** BigLake managed Iceberg, because the whole point of promoting to Iceberg is time-travel + schema
evolution; option 2 throws both away. The writer should emit standards-compliant Iceberg metadata regardless, so the
catalog choice does not change the bytes on GCS — only how BigQuery is pointed at them.

> **Flag for the operator:** confirm BigLake-managed-Iceberg vs external-tables-over-Parquet before the writer commit.
> This is the one externally-constrained decision; the rest of this doc holds either way.

## Retention

- **Data + manifests:** keep **10 years**, matching the matter-file retention the client consents to in the retainer
  (`projects.closed_at + 10y`, see <surreal-archives.md>). The `application/` lane (binding SurrealDB entity tables)
  carries no GCS lifecycle rule — retention is enforced by never pruning the snapshot log. Only the content-free
  `telemetry/otel_logs/` prefix has a lifecycle rule, transitioning to Coldline at 365 days and deleting at 10 years
  (`examples/deploy/k8s/observability/exports-bucket-lifecycle.json`); `telemetry/otel_traces/` and
  `telemetry/otel_metrics/` delete at 30 days.
- **Snapshot expiry:** Iceberg snapshot-expiry (dropping old manifest entries) is **not** run — the snapshot log is the
  point-in-time index we want. Revisit if the snapshot metadata grows unwieldy; retention changes require a matching
  reader and reconstruction drill.

## Where it runs

The PostgreSQL coverage generation runs on the **DBOS executor inside `web`**. That is the process `ops ship` already
gives the PostgreSQL connection and the DBOS login, and it moves the schedule off Restate. Set
`NAVIGATOR_DBOS_ARCHIVES_ENABLED = "true"` in the deployment's `config.toml`, beside `NAVIGATOR_POSTGRES_ENABLED` and
`NAVIGATOR_DBOS_ENABLED`. `ops ship` then:

1. Renders the switch, `NAVIGATOR_ARCHIVES_BUCKET`, and the messages channels into `web`.
2. Adds `examples/deploy/k8s/gke/postgres/web/web-archives.yaml`. It gives `web` the read-only `navigator_archive` login
   for both the `navigator` and `forgejo` databases, from the `navigator-postgres-archive` Secret.
3. Adds the `navigator-archive-coverage` ConfigMap from `k8s/postgres/archive`, read as `NAVIGATOR_ARCHIVE_COVERAGE`.
4. Drops the Restate `archives-trigger` CronJob from the render and deletes the live one after the roll.

`ops ship` refuses the switch without `NAVIGATOR_POSTGRES_ENABLED` and `NAVIGATOR_DBOS_ENABLED`, and refuses to apply
without `NAVIGATOR_POSTGRES_ARCHIVE_PASSWORD` in the PostgreSQL service file and its Secret in the namespace. `web`
refuses to boot with the switch on and any coverage setting or the Slack bot missing. The run keeps the Restate
workflow's slots, `Archives:<UTC slot>` workflow ids, and step names (`snapshot`, `cost`, `iceberg_telemetry`, `email`).
Each successful slot publishes `postgres/_paired/<generation>.json` last.

With the switch off, the `archives-trigger` CronJob still starts the Restate workflow on `workflows-service`. That pod
has no PostgreSQL login, so it exports SurrealDB and telemetry only.

The `navigator_archive` login holds `pg_read_all_data` and `CONNECT` on both databases. It reads every relation the
manifest names, including `dbos`, AGE's catalogs, the migration ledger, and large objects, and it can write nothing. The
`navigator_app` runtime login cannot read those relations, so it cannot run the export. `k8s/postgres/access/roles.sql`
creates the archive login once its Secret is mounted.

Each extraction holds both databases' Parquet in memory before it uploads. Watch `web`'s memory on the first scheduled
runs, and raise its request if a run approaches it.

### The coverage manifest

`k8s/postgres/archive/coverage.json` is the reviewed coverage manifest. It has a `navigator` half and a `forgejo` half,
and each lists every relation its database holds. A relation the manifest does not name fails the generation, so a
change that adds one updates this file in the same pull request:

- **A migration, or a DBOS SDK upgrade.** `archives/tests/coverage_manifest.rs` migrates a fresh database, launches
  DBOS on it, and checks the `navigator` half against what it finds. When it fails, it prints a draft of the observed
  inventory. Review the draft and copy the changed relations into the file.
- **A Forgejo upgrade.** Start the new Forgejo image against a disposable PostgreSQL database and let it migrate. Then
  run `cargo run -p archives --example coverage_inventory -- <url of that database>` and review the `forgejo` half
  against the output. CI has no Forgejo database, so this check is manual. A missed relation fails the scheduled run
  closed and posts the failure to Slack.

A draft gives every view an empty reconstruction reason, and the archive refuses an empty reason. A reviewer must write
how each view is rebuilt before it can ship. `person_directory` and `firm_visible_person` are derived this way: each is
a plain view over exported tables, rebuilt by its migration. PostGIS's `geometry_columns` and `geography_columns` are
derived from the system catalogs and recreated by `CREATE EXTENSION postgis`; its `spatial_ref_sys` table is exported.

## Recovery: physical backups first

The Iceberg archive is the **logical and analytical copy** of PostgreSQL. It is not the recovery path for the
self-hosted server that runs AGE and DBOS:

- **Recover the server from physical backups.** The WAL archive and the daily base backup in `k8s/postgres/backup`,
  proven by the scratch restore drill in `k8s/postgres/restore-drill`, restore both databases to one point in time. They
  also keep DBOS's pending workflows resumable on the same executor identity and release ([PostgreSQL on
  Kubernetes](postgres-kubernetes.md#backups-and-point-in-time-recovery)).
- **Iceberg does not certify DBOS replay.** It holds the `dbos` tables as rows. Rehydrated rows are a record of what
  ran, not a system database an executor can resume from.
- **Iceberg has no shared recovery point.** Each database has its own watermark (`cross_database_atomic: false`), and
  sequences are read outside MVCC.

Use the Iceberg archive for queries, audits, long-term retention, and row-level recovery into an isolated database. Use
the physical backups to bring the server back.

## Post-provisioning drill

CI proves the writer, the attempt-key scheme, and the project seam against fakes; a locked bucket in a second GCP
project has no test double. After provisioning the offsite lane and before relying on it, run this drill once — it is
the manual proof CI cannot run. It uses synthetic names (`example-archive-project`, `example-archives-offsite`) because
real identifiers never enter Git ([public contributor safety](public-contributor-safety.md)); substitute your
deployment's real coordinates only at the CLI, never in a committed file.

1. **Provision — create → verify policy → lock.** Run `navigator ops gcp setup` with
   `--offsite-archives-project-id example-archive-project --offsite-archives-bucket example-archives-offsite`. The
   provisioner creates the bucket with its retention policy already in place, verifies the policy against the live
   bucket, and only then locks it — the lock is irreversible, so a wrong policy is converged by deleting the bucket
   before it was ever locked, never by "fixing" a locked one. The workflows-service identity receives
   `roles/storage.objectCreator` and `roles/storage.objectViewer` on the bucket and nothing else.
2. **Configure.** Set `NAVIGATOR_ARCHIVES_BUCKET` to the offsite bucket and `NAVIGATOR_ARCHIVES_GCP_PROJECT_ID` to
   the archive project's id or numeric project number on the workflows-service pod (see the example deployment's
   [`config.toml`](../deployments/example-deployment/config.toml)).
3. **Prove the fail-closed seam first.** Temporarily set `NAVIGATOR_ARCHIVES_GCP_PROJECT_ID` to a wrong project
   number and trigger one run: lane resolution must refuse before any object is written, and the run must record the
   failure. Restore the correct value.
4. **Run one archive cycle.** Trigger the scheduled workflow by hand —
   `kubectl --namespace navigator create job --from=cronjob/archives-trigger archives-drill-<date>` (see [scheduled
   jobs](cronjobs.md)) — and watch the Slack digest: the `archive project` line, the pinned per-table links, and no
   failure lines.
5. **Read back through Iceberg.** In the GCS browser pinned to `example-archive-project`, confirm the table's
   `metadata/` prefix holds `v1.metadata.json` (and no `version-hint.text`), with the data file under
   `data/dt=<date>/run=<HHMM>/snapshot.parquet`. Read the table through the catalog choice recorded above (BigLake or an
   external table) and confirm the row count matches the digest line.
6. **Prove immutability.** Attempt to replace and then delete the snapshot Parquet; GCS must reject both under the
   locked policy.
7. **Record the drill.** Date, digest message, snapshot id, and the read-back row count go in the deployment's
   operational log. The drill proves the offsite lane end to end; it does not replace the PostgreSQL rehydration drill
   or the separate recovery contracts noted under [PostgreSQL coverage](#postgresql-coverage-generations).

## Council consensus (folded in)

Run inline; the deliberation is not kept, only the decisions:

- **Aquarius (systems):** reuse the data files — the promotion is a metadata-layer add, not a re-snapshot. Adopted (see
  "What promote means").
- **Scorpio (trust):** the lake carries privileged content (`documents`, `notations`, `relationship_logs`). The
  `<project>-exports` bucket stays private (Workload Identity, no public binding) exactly as today; BigLake access is
  IAM-gated to the BI service account, never `allUsers`. Adopted — no public exposure in any option.
- **Capricorn (maintainability):** do **not** hand-roll Avro manifest writing if a maintained Rust Iceberg crate
  (`iceberg-rust`) covers the metadata spec; the writer commit evaluates it before writing manifest serialization by
  hand. Recorded as the first task of the follow-up.
- **Cancer (beginner's mind):** the `dt=<date>` rename is the only change visible to a reader; call it out so a future
  reader isn't surprised the directory shape shifted. Done (Partitioning).
- **Sagittarius (big picture):** deriving the lake from the shipped Surreal schema closes the archive-coverage gap
  without another table registry. Adopted (Tables).

## Writer follow-up — status

1. ~~Evaluate `iceberg-rust`~~ — **done**: chosen (see Evaluation outcome above); not hand-rolled.
2. ~~Restore the SurrealDB analytical snapshot source.~~ **Done**: every `DEFINE TABLE` is discovered from the shipped
   schema and read through one generic query path.
3. ~~Emit `metadata/` (table metadata JSON + manifest list + manifest), append-only~~ — **done** as a reusable
   writer (`archives::iceberg::author_snapshot`, unit-tested for metadata round-trip + snapshot-log chaining), and
   **wired for both lanes** via the `iceberg_telemetry` workflow phase: every `ALL_TABLES` entry under `application/`
   and `otel_logs`/`otel_traces`/`otel_metrics` under `telemetry/`.
4. ~~Rename the data partition dir to `dt=<date>`~~ — **done**, together with the `application`/`telemetry` lane
   prefix (`archives/src/snapshot.rs`, `archives/src/drift.rs`, `archives/src/iceberg.rs`). The table is still
   unpartitioned in the Iceberg partition spec (see v1 simplifications above); only the object-key directory changed.
5. Wire BigLake (or external tables, per the operator's call) and add a smoke query. **Pending** (machine-bound; the
   one offline-unverifiable part — needs a live BigQuery to confirm the authored metadata is BigLake-readable).
6. Surface per-table Iceberg snapshot ids in the scheduled diagnostic email. **Pending** (with the wiring in step 3).
