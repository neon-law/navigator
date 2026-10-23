---
name: public-repositories
description: Check or configure Navigator's public-repository governance posture.
---

# Public repository posture

Read [`docs/licensing.md`](../../../docs/licensing.md), [`docs/gitops.md`](../../../docs/gitops.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md) before changing repository settings.

- Keep the licence, notice, contribution policy, and merge rules consistent with this repository's canonical files.
- Read the live repository state before proposing any mutation. Make no production or irreversible change without
  explicit user authorization.
- A public source repository holds code and synthetic or firm-owned examples only. Do not add client data, legal
  files, real contact details, production identifiers, or operational maps.
- A **publish workflow in a public repository** grants `id-token: write` and writes its coordinates into a public
  Actions log. Pass the deployment's applications bucket, publisher service account, and Workload Identity provider as
  repository **secrets**, never variables: GitHub redacts a secret's text but not a variable's, and two of the three
  contain the deployment's GCP project identity. Secrets reduce disclosure; the Workload Identity binding on Google's
  side is the access control. See [`docs/project-repositories.md`](../../../docs/project-repositories.md).
- An IAM condition confines a Project's publisher to **one** `<code>/portal` prefix, and its service account derives
  from the deployment's GCP project id, so one deployment carries one Project's portal publisher. Provisioning a second
  Project against the same publisher is refused, because repointing would silently revoke the first Project's publish.
  Give the second Project its own publisher identity.
