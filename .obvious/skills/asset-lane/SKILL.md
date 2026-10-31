---
name: asset-lane
description: Runbook for uploading public images and fonts to the GCS assets bucket via `navigator ops assets upload` — ADC/GCS credentials, bucket naming, and troubleshooting.
version: 1.0.0
triggers:
  - upload a photo to the asset lane
  - upload image to assets bucket
  - upload fonts to the bucket
  - navigator ops assets upload
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
`docs/assets.md`. The bytes themselves are published to the public GCS assets
bucket (`<project>-assets`) through the asset lane, and served to brand sites
from there.

This skill covers the operator upload commands, the credentials they need,
and the failure modes seen in practice.

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

### Publish a brand photo (image variants)

1. Register the asset key in `views/src/assets.rs` and add it to
   `cli/src/assets.rs` `reachable_image_keys`; add provenance to
   `docs/assets.md`. Commit and merge those references first.
2. Prepare the variants in a local directory (one image file per variant
   the site expects, e.g. the hero and thumbnail sizes).
3. Upload:
   ```bash
   export NAVIGATOR_ASSETS_BUCKET="$SECRET_NAVIGATOR_ASSETS_BUCKET"
   export GOOGLE_APPLICATION_CREDENTIALS="$KEYFILE"
   cargo run -p cli -- ops assets upload --dir ./variants
   ```
   The bucket defaults to `NAVIGATOR_ASSETS_BUCKET`; `--bucket` overrides it.
   Objects publish under `img/` in the bucket.
4. Verify the site CSP serves the image (self-hosted bucket origin only —
   remote URLs are stripped by CSP).

### Publish a font family

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
| `no bucket — pass --bucket or set NAVIGATOR_ASSETS_BUCKET` | Bucket env var unset/empty | Export `NAVIGATOR_ASSETS_BUCKET` (secret `SECRET_NAVIGATOR_ASSETS_BUCKET`) |
| `open bucket` error mentioning credentials/ADC | `GOOGLE_APPLICATION_CREDENTIALS` unset or key file invalid | Write `SECRET_GCS_SERVICE_ACCOUNT_JSON` to a file and export its path |
| ADC refuses, falls back to `gcloud` | No service-account key configured | Configure the key file; `gcloud application-default login` is the interactive fallback |
| Upload succeeds but image does not render | Site CSP restricts images to self and data | Confirm the asset is served from the first-party bucket, not a remote origin |
| `403` on upload | Service account lacks `storage.objects.create` | Grant the role on the assets bucket |
| Bytes accidentally in git tree | Added under `server/public/img` | Remove from the commit; keep only the asset key registration + `docs/assets.md` provenance |
