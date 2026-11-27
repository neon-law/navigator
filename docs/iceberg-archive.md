# PostgreSQL Iceberg archives

Iceberg generations provide logical exports for PostgreSQL and Forgejo. Use the independent physical backup and WAL path
to recover the PostgreSQL server. The application has no scheduled Iceberg export job.

## Exporting a generation

`archives::postgres::export_generation` exports a named relation set. Use `archives::coverage::export_pair` for a paired
generation. The paired export checks the reviewed relation inventory, extracts each database through a repeatable-read
transaction, and publishes the paired marker after writing both sets of objects. Each database has its own read
watermark. The marker carries object digests for verification.

## Recovery: physical backups first

The Iceberg archive is the **logical and analytical copy** of PostgreSQL. It is not the recovery path for the
self-hosted server that runs AGE and DBOS:

- **Recover the server from physical backups.** The WAL archive and the daily base backup in `k8s/postgres/backup`,
  proven by the scratch restore drill in `k8s/postgres/restore-drill`, restore both databases to one point in time. They
  also keep DBOS's pending workflows resumable on the same executor identity and release ([PostgreSQL on
  Kubernetes](postgres-kubernetes.md#backups-and-point-in-time-recovery)).
- **Iceberg does not certify DBOS replay.** It holds the `dbos` tables as rows. Restored rows alone do not establish
  safe executor resumption or external-effect replay.
- **Iceberg has no shared recovery point.** Each database has its own watermark (`cross_database_atomic: false`), and
  sequences are read outside MVCC.

Use the Iceberg archive for queries, audits, long-term retention, and row-level recovery into an isolated database. Use
the physical backups to bring the server back.

### Native logical recovery

A [Database Recovery](glossary/database-recovery.md) binds an archive to a newly created target with its own Database
Origin. The archive writer and the write-only verifier do not activate a target. Recovery uses a separately authorized
reader that can fetch and list archive objects; the create-only backup credential does not provide that access.

Create the target through `native_origin::create` and `initialize_recovery` with the actual paired marker. Prepare the
matching DBOS schema without running workflows. `archives::recovery::rehydrate` restores rows while admission remains
blocked. `verify_and_complete` seals the resource, independently checks its schema, relation inventory, data and
sequence safety, and records completion atomically. The target keeps its own creation identity and real migration
history; archived source authority stays provenance. Runtime connection grants and workers require a separate operator
action.

Native activation requires a supported source plan and matching target shapes and application routine definitions.
Captured-source and unsupported upgrade archives retain their stored data and authority evidence without an activation
plan. Materialized views, nondeferrable cross-table cycles, standalone aggregates and window functions, nondefault
large-object ownership or ACLs, and objects exceeding the reader's declared limits require their own recovery procedure.
Verification refusals after sealing keep connections closed. An unconfirmed commit or reopen requires explicit
inspection before proceeding.

The native ownership drill exercises three consecutive recovery generations through memory, file and GCS-style URIs
backed by local storage. It restores real SDK checkpoints without repeating completed work. This does not establish live
GCP access, external-effect replay, Forgejo service recovery or a shared recovery point.
