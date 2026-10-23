---
name: public-images
description: Add or maintain public image references using Navigator's approved asset workflow.
---

# Public images

Read the relevant content source, [`docs/assets.md`](../../../docs/assets.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- Use only rights-cleared, public-safe images with accurate alt text. Keep local assets and captures out of Git where
  the asset workflow requires it.
- Verify the image reference locally. Do not deploy or mutate cloud storage without explicit authorization.
- Publish a single asset to staging and production together with `navigator site asset upload`. The path is relative
  to `server/public/` and is also the bucket key. Each host needs a current `navigator site login` (one-hour token):

  ```bash
  navigator site whoami --host staging.neonlaw.com
  navigator site whoami --host www.neonlaw.com
  navigator site asset upload --host staging.neonlaw.com --host www.neonlaw.com img/<slug>/<file>
  ```

  `uploaded` or `unchanged` means the CLI already read `<host>/assets/<key>` back byte for byte. Rerun only a failed
  `--host`; report a host you could not reach as pending, never as published. Use `ops assets upload` only for batch
  gallery publication.
- A crop, blur, or redaction never licenses publishing client material, legal files, real contact details, or
  production identifiers.
