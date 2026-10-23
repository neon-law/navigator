---
name: authoring-slides
description: Create or structurally repair Navigator workshop and presentation Markdown.
---

# Author slides

Read the relevant workshop source, [`docs/agent-workflows.md`](../../../docs/agent-workflows.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- Preserve an existing deck's spoken words and notes. Change only structure or typography unless its author approves
  the copy change.
- Follow the loader's required frontmatter, chapter, slide, and notes shape; run the CLI validator on the changed file.
- Use repository-owned, public-safe media only. Never put legal files, client imagery or data, real contact details, or
  production identifiers on a slide, in its source, or in a capture.
- Keep a bucket-lane slide image's ignored local copy at `server/public/img/<deck-slug>/<filename>`, then publish it to
  staging and production in one call, as the `public-images` skill shows:

  ```bash
  navigator site asset upload --host staging.neonlaw.com --host www.neonlaw.com img/<deck-slug>/<filename>
  ```

- If a host remains pending, say so; do not describe a staging-only image as published everywhere.
- Keep generated captures in `/tmp`; use the asset workflow for approved public media.
