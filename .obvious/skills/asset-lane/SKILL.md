---
name: asset-lane
description: Runbook for publishing public images and fonts to a deployment's public assets bucket — the primary path is `navigator site asset upload` with a `site login` bearer (no GCS credentials); `navigator ops assets upload` with ADC/GCS credentials remains the bulk/variants fallback.
version: 1.1.0
triggers:
  - upload a photo to the asset lane
  - upload image to assets bucket
  - upload fonts to the bucket
  - navigator ops assets upload
  - navigator site asset upload
  - navigator site login
  - asset lane upload
  - GCS credentials for assets
  - GOOGLE_APPLICATION_CREDENTIALS for navigator
  - NAVIGATOR_ASSETS_BUCKET
  - publish brand image
  - self-host a photo for a brand page
  - CSP blocks the image
  - where do public images live
author: Neon Law
category: devops
---

# asset-lane

## Overview

Public image and font bytes are **not stored in Git**. The tracked tree carries only
references: an asset key in `views/src/assets.rs`, reachability in
`cli/src/assets.rs` (`reachable_image_keys`), and provenance notes in
`docs/assets.md`. The bytes themselves are published to the public assets bucket
and served to brand sites from there.

Two upload paths exist:

1. **`navigator site asset upload` — the default.** Publishes one asset through a
   live deployment's own HTTP API (`POST /app/api/assets`). It needs **no bucket
   name and no GCP credential** — only a `navigator site login` bearer token —
   and it verifies itself: after the server accepts the write, the CLI reads the
   object back through the deployment's public `/assets/{key}` origin (the same
   route a browser uses) and fails unless the served bytes are identical.
   Prefer this path for any single-asset publish.
2. **`navigator ops assets upload` — the bulk fallback.** Uploads a whole
   directory of variants (e.g. hero + thumbnail sizes) or a font family directly
   to GCS through Application Default Credentials. Use it when one bearer-token
   request per file is wrong — multi-file variant sets and font directories.

Both paths write to the same bucket, so the two are interchangeable per object.

## When to Use

- A brand page references a photo/font that must be self-hosted (the site CSP
  restricts images to self and data, so remote hotlinks do not render).
- Registering a new asset for an existing brand and publishing its bytes.
- Uploading a WOFF2 font family for a bucket-served typeface.
- Debugging an upload that fails with an auth or bucket error.

## When NOT to Use

- Private documents (they go to the *private* documents bucket,
  `NAVIGATOR_DOCUMENTS_BUCKET`, behind the authorization gate — never the
  public assets bucket).
- Storing image bytes in Git — the repo must not carry public asset bytes.

## Credentials

### Path 1 (primary): a `navigator site login` bearer token

`navigator site login <host>` runs a browser-loopback authentication flow
against a live deployment and stores a **short-lived (1 hour)** bearer token at
`~/.navigator.json` (mode `0600`). Publishing an asset through
`navigator site asset upload` requires the stored identity to hold the
**Owner or Admin** tier on that deployment — a lawyer-tier login is refused.

- The token expires quickly by design; log in again when a run reports `401`.
- Tests and scripts can redirect the credential file with
  `NAVIGATOR_CREDENTIALS_FILE`; the file layout is
  `{"hosts": {"<base-url>": {"token", "person_email", "role", "expires_at"}}}`.
- No secrets-manager entry is needed for this path — the operator's own login
  is the credential.

### Path 2 (fallback): ADC / GCS service account

`navigator ops assets upload` targets **real GCS through ADC**
(`cloud/src/gcs.rs`, `GcsStorage::new_from_config`). There is no API-key
path: authentication is Application Default Credentials, picking up
`GOOGLE_APPLICATION_CREDENTIALS` (the GCP convention) first and the
`gcloud` CLI second.

Obvious sandbox secrets (names are stored bare; injection adds the
`SECRET_` prefix):

- `SECRET_GCS_SERVICE_ACCOUNT_JSON` — a GCP service-account key in JSON
  format. Write it to a file and point `GOOGLE_APPLICATION_CREDENTIALS` at it:
  ```bash
  # Validate presence; never print the full key
  if [ -z "${SECRET_GCS_SERVICE_ACCOUNT_JSON:-}" ]; then
    echo "ERROR: SECRET_GCS_SERVICE_ACCOUNT_JSON not configured."
    echo "Request it via obvious secrets upsert --name=GCS_SERVICE_ACCOUNT_JSON."
    exit 1
  fi
  KEYFILE=$(mktemp /tmp/gcs-sa-XXXXXX.json)
  printf '%s' "$SECRET_GCS_SERVICE_ACCOUNT_JSON" > "$KEYFILE"
  export GOOGLE_APPLICATION_CREDENTIALS="$KEYFILE"
  # Validate it parses (shows the account, not the key material)
  jq -r .client_email "$KEYFILE"
  ```
- `SECRET_NAVIGATOR_ASSETS_BUCKET` — the public bucket name
  (`<project>-assets`). Export it as `NAVIGATOR_ASSETS_BUCKET`.

The service account needs `storage.objects.create` (+ `storage.objects.list`
to verify) on the assets bucket.

**Emulator exception:** when `NAVIGATOR_STORAGE_ENDPOINT` is set, the storage
client targets the emulator with anonymous credentials and never touches ADC.
Local dev against the emulator needs no credentials.

## Workflows

### Publish a brand asset (single file, bearer path)

1. Register the asset key in `views/src/assets.rs` and add it to
   `cli/src/assets.rs` `reachable_image_keys`; add provenance to
   `docs/assets.md`. Commit and merge those references first.
2. Log in to the deployment that serves the brand's bucket (an Owner/Admin
   login — production and staging deployments of the platform share one
   assets origin, so logging in to any one of them is sufficient):
   ```bash
   cargo run -p cli -- site login www.neonlaw.com
   ```
3. Place the file under `server/public/<key>` in the repo root and upload it by
   its key-relative path — the object publishes under exactly that key:
   ```bash
   cargo run -p cli -- site asset upload --host=https://www.neonlaw.com \
     img/<brand>/courthouse-columns.jpg
   ```
   - The key is the asset's path relative to `server/public/`; an asset that
     lives elsewhere needs an explicit `--key img/<brand>/name.jpg`.
   - Pass `--host` once per deployment to publish to several; each host is an
     independent write, and the command exits non-zero naming any host that
     failed (hosts that succeeded before the failure stay published).
   - Content type is derived from the extension (png/jpg/webp/avif/svg/ico,
     mp4, woff/woff2); an unrecognized extension is refused before any request.
4. Success output includes the served-byte verification: the command reads
   `{base}/assets/{key}` back and exits non-zero if the deployment serves
   different bytes. Confirm the site CSP serves the image (self-hosted bucket
   origin only — remote URLs are stripped by CSP).

### Publish a directory of image variants (bulk fallback)

```bash
export NAVIGATOR_ASSETS_BUCKET="$SECRET_NAVIGATOR_ASSETS_BUCKET"
export GOOGLE_APPLICATION_CREDENTIALS="$KEYFILE"
cargo run -p cli -- ops assets upload --dir ./variants
```
The bucket defaults to `NAVIGATOR_ASSETS_BUCKET`; `--bucket` overrides it.
Objects publish under `img/` in the bucket.

### Publish a font family (bulk fallback)

```bash
export NAVIGATOR_ASSETS_BUCKET="$SECRET_NAVIGATOR_ASSETS_BUCKET"
export GOOGLE_APPLICATION_CREDENTIALS="$KEYFILE"
cargo run -p cli -- ops assets fonts upload --dir ./woff2
```

Font files use the same public assets bucket, under a `fonts/<dir>/` prefix
per family. The family must be a row in `BUCKET_FONT_FAMILIES`
(`cli/src/assets.rs`).

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `401` from `POST /app/api/assets` | Bearer token expired (site logins last ~1h) | `navigator site login <host>` again |
| `403` from `POST /app/api/assets` | Login is below Owner/Admin tier, or token is for the wrong host | Log in as an Owner/Admin on the `--host` deployment |
| `{base} accepted the upload but {url} serves different bytes` | Read-back verification failed | Re-run the upload; if it repeats, check the deployment's asset origin/routing before retrying |
| `no bucket — pass --bucket or set NAVIGATOR_ASSETS_BUCKET` | Bucket env var unset/empty (bulk path only) | Export `NAVIGATOR_ASSETS_BUCKET` (secret `SECRET_NAVIGATOR_ASSETS_BUCKET`) |
| `open bucket` error mentioning credentials/ADC | `GOOGLE_APPLICATION_CREDENTIALS` unset or key file invalid (bulk path only) | Write `SECRET_GCS_SERVICE_ACCOUNT_JSON` to a file and export its path |
| ADC refuses, falls back to `gcloud` | No service-account key configured | Configure the key file; `gcloud application-default login` is the interactive fallback |
| Upload succeeds but image does not render | Site CSP restricts images to self and data | Confirm the asset is served from the first-party bucket, not a remote origin |
| `403` on bulk upload | Service account lacks `storage.objects.create` | Grant the role on the assets bucket |
| Bytes accidentally in git tree | Added under `server/public/img` | Remove from the commit; keep only the asset key registration + `docs/assets.md` provenance |
