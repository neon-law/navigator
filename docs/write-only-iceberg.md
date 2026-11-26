---
title: Write-only Iceberg archives
---

# Write-only Iceberg archives

DBOS Archives seals one generation in Google Cloud Storage and creates independent offsite attempts. Its source-store
reader explicitly selects GKE metadata or a credential file. Offsite handles use Application Default Credentials (ADC)
and send conditional object creates only. The verifier runs separately with read/control-plane access.

The cloud deployment needs no persistent filesystem, Filestore, PVC, or additional always-on service. Extraction and
encoding use bounded memory. Object storage holds the sealed rows, immutable metadata, attempt fences and transport
receipts; DBOS checkpoints their exact identities. A receipt is never permission to re-extract changed rows.

## Configure the object-backed source

Use an existing operator-approved bucket and a dedicated prefix. Its reader needs exact-object reads and immutable
creates in that prefix, without destination read access. Keep the source bucket distinct from every offsite bucket.
Review the prefix's retention and referenced-generation policy before activation; a telemetry or expiring PITR prefix is
not an interchangeable archive source. These operations apply no lifecycle, deletion or retention changes.

The source project number identifies the original PostgreSQL project, not the working-store bucket's project. The
working bucket can share an archive project with an offsite destination when their buckets are distinct and the source
and destination permissions are independently reviewed.

Prepare a private source-store JSON file. The identifiers below are synthetic:

```json
{
  "bucket": "example-sealed-archive-source",
  "prefix": "sealed-generations",
  "authentication": "gke_metadata",
  "max_generation_bytes": 67108864,
  "max_object_bytes": 16777216,
  "max_objects": 4096,
  "max_rows_per_table": 100000
}
```

GKE metadata mode obtains a short-lived token directly from the pod's metadata server. It ignores ADC credential files
and never invokes `gcloud`; it requires no service-account key, projected-token file or source credential Secret. Grant
the configured KSA/GSA source-prefix read/create permission. The same principal may have offsite create-only permission
without destination reads; separate clients enforce their operation capabilities.

For an operator's explicit credential file, use `"authentication": "credentials_file"` and an absolute
`"credentials_file"` path. Supported federation JSON exchanges a separately provisioned subject token through STS;
configure its audience/provider and token source explicitly. Enabling federation support does not provision a GKE
projected token or prove that its audience is accepted. File mode never changes process ADC or falls back to `gcloud`,
and its file must differ from explicit writer ADC. Keep actual identifiers, tokens and receipts private.

Cloud rendering defaults to `gke_metadata`. Set deployment coordinate `NAVIGATOR_ARCHIVE_SOURCE_AUTHENTICATION` to
`credentials_file` only for the reviewed file mode; its separate patch mounts the required
`navigator-archive-source-credentials` Secret at `/var/run/navigator-archive-source/credentials.json`. Ship refuses a
private source configuration whose mode differs from the rendered selection. Metadata mode mounts configuration only.

Prepare a destinations JSON array with independently owned projects and distinct buckets:

```json
[
  {
    "project_number": 100000000001,
    "bucket": "example-independent-archive-a",
    "location": "us-west4",
    "minimum_retention_seconds": 788940000
  }
]
```

Bind admitted Navigator and Forgejo identities and the reviewed paired coverage file:

```bash
hudson ops archives config --object-store source-store.json \
  --source-project-number "$SOURCE_PROJECT_NUMBER" --destinations destinations.json \
  --coverage coverage.json --out archive-config.json
```

Version 2 identifies object-backed configuration and carries no local root. Version 1 retains explicit local backups;
versions and receipt backing are checked rather than interpreting a filesystem path as a GCS location. Each paired
capture records individual database snapshots, coverage and non-MVCC repair obligations. It is not cross-database
atomic.

## Capture and recover an exact generation

Give a CLI capture an explicit stable identity. The DBOS schedule uses its existing workflow identity. GKE metadata
capture runs in the deployed executor or an approved GKE operator runtime. An operator machine uses explicit-file source
authentication and separate writer ADC; setting writer ADC does not provide a GKE metadata endpoint:

```bash
GOOGLE_APPLICATION_CREDENTIALS="$WRITER_ADC" hudson ops archives capture \
  --config archive-config.json --capture-id "$CAPTURE_ID" --out capture-report.json
hudson ops archives verify-sealed --config archive-config.json \
  --receipt capture-report.json --out source-verification.json
```

Use a new private report path for each invocation; Hudson refuses to overwrite reports. A source generation is committed
only after all objects and its versioned seal are stored. Source readback can resolve the source's lost create
acknowledgement. The stable capture identity locates that seal after a process or checkpoint loss, with exact object
versions, hashes, cutoffs and source fingerprints. A capture fence without a complete seal refuses extraction; a fresh
cutoff requires a new explicit capture identity.

Prepared destination metadata references that destination's own Parquet paths. Its source-store receipt references the
original frozen Parquet versions, so preparation does not store another full source data copy. Offsite metadata never
references source-bucket data. Source reads validate the complete closure before destination work.

Each destination attempt has a durable source-store fence. An acknowledged outcome survives a lost DBOS checkpoint. An
upload without a durable outcome stays unconfirmed and is not automatically repeated. The writer never probes the
destination to resolve uncertainty; the separate reader does that. For an explicit retry, reuse the sealed report and
select a new delivery attempt, from 1 through 3:

```bash
GOOGLE_APPLICATION_CREDENTIALS="$WRITER_ADC" hudson ops archives capture \
  --config archive-config.json --capture-id "$CAPTURE_ID" --reuse-sealed capture-report.json \
  --delivery-attempt 1 --out retry-report.json
```

The retry preserves the original rows and cutoff. It does not open PostgreSQL or recapture an unavailable generation.
Upload acknowledgement proves transport, not remote verification, catalog publication or restore acceptance.

## Deploy with DBOS Archives

Select the existing Archives schedule in the deployment's private configuration:

```toml
NAVIGATOR_DBOS_ENABLED = "true"
NAVIGATOR_DBOS_ARCHIVES_ENABLED = "true"
NAVIGATOR_ARCHIVE_WRITE_ONLY_CONFIG = "/var/lib/navigator/iceberg/config/archive-config.json"
```

Provision `navigator-archive-write-only` as a ConfigMap with the non-secret `archive-config.json` key. Metadata mode
mounts configuration only and uses the existing KSA/GSA identity. For explicit-file mode, select
`NAVIGATOR_ARCHIVE_SOURCE_AUTHENTICATION = "credentials_file"` and provision `navigator-archive-source-credentials`, key
`credentials.json`; its separate read-only patch lets group 65532 read the credential file. Keep verifier credentials
out of `web`. Cloud preflight requires version 2, matching identity modes and an independent destination; only file mode
requires the mounted credential path and Secret. Local backups remain an operator CLI mode.

Before enabling the PostgreSQL write-only schedule, verify admission, archive database readers, reviewed coverage,
source-store IAM, offsite writer ADC and native worker configuration. The renderer removes the Restate Archives trigger
when DBOS Archives is selected.

The authenticated Archives **Run now** control uses the locally registered workflow in the lightweight web host. It
freezes the same mounted configuration as a scheduled run and starts a distinct `Archives:manual-<uuid>` identity. It
needs no worker ingress URL or bearer token. The combined host uses its authenticated DBOS ingress. A selected local
owner that cannot admit the run refuses it; it does not fall back to another executor or Restate. A queued notice proves
admission only. Inspect the exact workflow and completed report before verification or recovery acceptance.

## Keep storage and retry costs bounded

The configured byte, row and object limits fail closed. Raw source sizes are checked inside the extraction snapshot
before decoding; encoded buffers and the total generation are bounded too. One extraction gate per source prevents
concurrent replica extraction. Destination preparation and upload are sequential, and each delivery attempt is fenced.
No background loop creates more copies after a lost acknowledgement.

Account for object count and API operations as well as bytes. Independent Iceberg tables need metadata and Avro objects;
source verification uses metadata and generation-pinned reads. The source stores frozen Parquet once and destination
metadata separately; each offsite attempt writes a complete independent copy. Explicit retries and partial attempts can
therefore increase retained objects even when their source rows are shared.

Review storage class, minimum storage duration, early-deletion charges, retrieval and verification reads, and regional
network transfer together. Small frequently verified archives can cost more in operations than stored bytes. This
transport does not select Coldline/Archive, alter lifecycle rules or delete referenced source generations. Retention
cleanup remains an independently authorized operation after all required references and recovery evidence are safe.

## Keep explicit local backups

For a raw local Iceberg copy, use a private persistent directory and version 1:

```bash
hudson ops archives config --local-root /private/archive-data \
  --registered-navigator-only --destinations destinations.json --out local-config.json
hudson ops archives capture --config local-config.json --out local-report.json
hudson ops archives verify-local --config local-config.json \
  --receipt local-report.json --out local-verification.json
```

`--registered-navigator-only` is deliberately narrower than paired coverage and does not capture the DBOS system
journal. Local reuse requires the original sealed files; a missing or changed file refuses recovery. `--reuse-local`
remains a spelling for `--reuse-sealed`, and `verify-local` remains a spelling for `verify-sealed`.

## Test, approve, then lock

Use separate private configurations and destinations for each environment. During testing keep destinations unlocked.
Run capture, verify the sealed source, restart/reuse it, and prove denied writer destination reads, listing, deletion,
updates, administration and cross-environment writes. Review inherited IAM and the live Workload Identity mapping. IAM
diagnostics alone do not prove an upload or restore succeeded.

The separate cloud verifier requires locked retention and refuses an unlocked test bucket. Source verification and
transport acknowledgement are separate evidence; do not weaken cloud verification to pass a test. Render the proposal:

```bash
hudson ops archives provisioning-plan --config archive-config.json \
  --writer "serviceAccount:$ARCHIVE_WRITER_EMAIL" \
  --verifier "serviceAccount:$ARCHIVE_VERIFIER_EMAIL" --out proposed-policy.json
```

The plan applies nothing. After capture and restart tests pass, obtain explicit approval of the bucket, owning project
and retention duration. The current minimum is 788940000 seconds, or 25 years. With separately authorized control-plane
credentials, set and read back the reviewed policy:

```bash
gcloud storage buckets update "gs://$ARCHIVE_BUCKET" --retention-period="$RETENTION_PERIOD"
gcloud storage buckets describe "gs://$ARCHIVE_BUCKET" --format=json
```

`RETENTION_PERIOD` uses seconds with an `s` suffix. Verify project, location, public access prevention, uniform access
and the exact period before the independently approved irreversible lock:

```bash
gcloud storage buckets update "gs://$ARCHIVE_BUCKET" --lock-retention-period
```

Read the locked policy back, then use the separate verifier ADC and the exact report:

```bash
GOOGLE_APPLICATION_CREDENTIALS="$VERIFIER_ADC" hudson ops archives verify-cloud \
  --config archive-config.json --destination 0 --report capture-report.json \
  --out offsite-verification.json
```

The verifier checks bucket policy, exact inventory, object hashes and the complete metadata/Avro/Parquet URI closure
before executing Iceberg scans. Reports keep `writer_authorization_review_required: true` and `catalog_promoted: false`.
Repeat independent reading and isolated restore acceptance in staging before production.

## Recovery scope

Structured tables do not copy Asset, document, attachment, Notation or Git bytes. Keep matching Git/LFS/object backups,
encryption keys and configuration. A logical origin row does not attest a newly created restore target. Restore
admission, physical PostgreSQL base/WAL restoration, DBOS recovery and provider-effect replay remain separate gates. No
archive report authorizes target writes, activation, deletion or provider-effect replay.
