# SurrealDB operational archives

The scheduled `surreal-archive` Kubernetes CronJob writes a recoverable SurrealQL artifact. It is separate from the
analytical Parquet/Iceberg `archives` lane.

## Storage and retention

Each cloud deployment sets `NAVIGATOR_SURREAL_ARCHIVES_BUCKET` to its own private retention bucket. Each new export has
a `.manifest.json` sidecar with its byte length and SHA-256, export-time table counts, and a hash of the complete
database/table schema inventory. Restore drills verify the downloaded bytes against that receipt and the restored
namespace against the receipt; they do not sample the live source as the expected state. The staging deployment uses
`neon-law-archives-staging`; the other deployment lanes use distinct operator-configured buckets and must not reuse this
value. Keys are point-in-time selectable:

```text
surreal-backups/<namespace>/<database>/<utc-timestamp>-<uuid>.surql
```

The bucket carries no lifecycle rule — objects are kept indefinitely — and stays private. The ten-year retention and
Coldline transition belong to the separate analytical Iceberg bucket instead; see
[`iceberg-archive.md`](iceberg-archive.md). The `workflows-service` workload identity needs `roles/storage.objectUser`
on the archives bucket; that IAM binding is an operator action.

## Restore drill

```bash
cargo run -p cli -- ops surreal-archive restore-drill \
    --key surreal-backups/navigator/navigator/<timestamp>-<uuid>.surql
```

The drill imports into a fresh `restore_<uuid>` namespace, applies `store/src/schema/navigator.surql`, and verifies
`schema_version:current`. It then reconciles every table's row count and schema fingerprint to the export receipt and
removes the scratch namespace. A failure exits non-zero so Kubernetes records a failed Job. The GKE
`SurrealArchiveFailed` managed-Prometheus rule alerts firm ops on a failed Job or a missing successful run for 26 hours;
configure its critical notification channel in the deployment project.

Routine scheduled exports remain available without a writer-quiescence assertion and are labeled `best_effort` in the
receipt. For a strict consistency proof, operators must first quiesce all application writers and then set
`NAVIGATOR_SURREAL_ARCHIVE_QUIESCED=true` for the full export command. Strict mode takes two complete exports and
pre/post schema and count inventories; any difference fails the operation. Setting the variable does not stop writes and
must never be added to a CronJob as a substitute for actual quiescence.

For an older artifact without a sidecar receipt, the drill retains a legacy path that compares the restored namespace to
the current live source. Its output explicitly says that it lacks an export-time receipt; it is recovery evidence only
and cannot establish source/target parity or cutover readiness.

For local verification, source the generated `.devx/env`; it explicitly points the archive lane at the worktree's shared
SeaweedFS exports bucket:

```bash
set -a; source .devx/env; set +a
cargo run -p cli -- ops surreal-archive export
cargo run -p cli -- ops surreal-archive restore-drill --key <printed-key>
```

## Derived tables

A derived table is a read-only view the schema computes from another table. `draft` projects `template` under the Draft
name, with `source_template_id` naming each row's canonical `template` record (see
[`store::drafts`](../store/src/drafts.rs)). The export leaves a derived table out, definition and rows both, and the
restore rebuilds it: `export` lists tables alphabetically, so a view is defined before the table it selects from, and
importing one fails and leaves a plain, writable table of the same name. Applying the schema after the import defines
the view and fills it from the restored `template` rows, and `apply` also replaces a `draft` that is not the shipped
view. The row-count reconciliation still counts the derived table, so the drill proves the rebuilt projection matches
the source. The analytical Parquet lane skips derived tables, because their rows are the same records as the source's
and would be counted twice.
