---
title: Write-only Iceberg archives
---

# Write-only Iceberg archives

This explicit Archives mode seals a persistent local Iceberg generation and creates independent offsite attempts. The
writer uses Application Default Credentials (ADC) without an ambient `gcloud` identity fallback. It sends only
conditional object creates. It does not read, list, delete, overwrite, inspect bucket policy, or publish a catalog.

The deployment operator supplies a read-only configuration file and a persistent private local archive mount. Do not use
a temporary directory for scheduled receipts. A lost or changed sealed generation fails recovery; replaying its workflow
never extracts a replacement snapshot under the old receipt.

## Configure and capture

Prepare a destinations JSON array with distinct projects and buckets. The following identifiers are synthetic:

```json
[
  {
    "project_number": 100000000001,
    "bucket": "example-independent-archive-a",
    "location": "us-west4",
    "minimum_retention_seconds": 788940000
  },
  {
    "project_number": 100000000002,
    "bucket": "example-independent-archive-b",
    "location": "us-central1",
    "minimum_retention_seconds": 788940000
  }
]
```

Use the existing reviewed paired coverage file to bind Navigator and Forgejo identities. Source readers use the existing
archive PostgreSQL and Forgejo reader credential configuration. The operator runs these commands with source admission
and reader access; those privileges are not granted to the cloud writer identity.

```bash
hudson ops archives config --local-root /var/lib/navigator/iceberg \
  --destinations destinations.json --coverage coverage.json --out archive-config.json
hudson ops archives capture --config archive-config.json --out capture-report.json
```

For deliberately narrower registered Navigator domain coverage, use `--registered-navigator-only` instead of
`--coverage`. This does not capture the DBOS system journal. A paired capture records individual database snapshots,
coverage and non-MVCC repair obligations; it does not claim a cross-database atomic snapshot.

Set `NAVIGATOR_ARCHIVE_WRITE_ONLY_CONFIG` to the operator-managed file path for the existing selected DBOS Archives
schedule. `NAVIGATOR_ARCHIVE_SOURCE` must be absent or `postgres`. Without this explicit configuration, the existing
archive mode and its guards remain in effect. Workflow input binds the configuration digest before uploading.

Each destination receives its own immutable attempt prefix and complete destination-specific Iceberg URI closure. The
final inventory is written last. An upload acknowledgement proves transport acknowledgement, not verification. Lost
acknowledgements remain unconfirmed. A repeated used attempt never probes remote existence with writer credentials. To
attempt delivery again, reuse the sealed generation and create new destination attempt keys:

```bash
hudson ops archives capture --config archive-config.json \
  --reuse-local capture-report.json --out retry-report.json
```

## Verify with a separate identity

The verifier uses a separately provisioned read/control-plane ADC identity. Do not put its credentials in the writer
process. It checks the destination project, location, uniform bucket access and locked retention, every object hash, and
the complete embedded metadata/Avro/Parquet URI closure before executing Iceberg scans. Unknown acknowledgements can be
resolved only by this reader. A policy check is not proof of effective inherited writer IAM.

```bash
hudson ops archives verify-local --config archive-config.json \
  --receipt capture-report.json --out local-verification.json
hudson ops archives verify-cloud --config archive-config.json \
  --destination 0 --report capture-report.json --out offsite-verification.json
hudson ops archives provisioning-plan --config archive-config.json \
  --writer serviceAccount:archive-writer@example.invalid \
  --verifier serviceAccount:archive-verifier@example.invalid --out proposed-policy.json
```

The provisioning plan applies nothing. The deployment operator separately validates independent project ownership,
uniform access, retention locking, effective inherited IAM and denied writer read/list/delete/overwrite operations. The
writer binding requests only `roles/storage.objectCreator`; verification and catalog promotion remain separate. Reports
always retain `writer_authorization_review_required: true` and `catalog_promoted: false`. The operator records any
separate acceptance outside these transport reports. No boolean in configuration substitutes for that evidence.

## Recovery scope

Verification reports contain the captured source fingerprints, per-database manifest where present, actual query counts,
and a structured restore scope. They authorize no target writes, activation, or provider-effect replay. Logical native
origin rows do not attest a different database as freshly created. Restore admission and physical identity acceptance
remain separate contracts.

Asset, document, attachment, Notation and Git bytes referenced by table rows are not copied by this structured lane. The
inventory distinguishes referenced payloads from copied payloads and marks dependency enumeration incomplete. Recovery
also needs the corresponding Git/LFS and object backups, encryption keys and credentials. Table readability does not
prove those dependencies survived loss of the primary project. Keep the physical base/WAL and DBOS recovery guards until
the required restore and effect-recovery scope has been independently demonstrated.
