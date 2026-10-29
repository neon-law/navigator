---
title: "Gate"
description: "The project gate checks a repository's Markdown, YAML, and seed documents against Navigator's rules."
---

`navigator project gate`, the command that checks one recognised repository — its authored Markdown, YAML, and seed
documents — against the Neon Law Navigator rule set, the same engine `navigator-lsp` runs on every keystroke. It takes
no path: it identifies the repository root by the `README` and `.git` beside it and refuses to run anywhere else. In a
Project repository it skips the Git-ignored document bytes under the root `documents/` directory, and validates each
committed [Pointer](pointer.md) under `blobs/` in a separate pass. Safe-by-construction fixes land as it goes; errors
fail the run and warnings print but pass. `--ci` writes no repository content, holds the origin pass to a built tree, on
a pull request refuses a [closing trailer](closing-trailer.md) the body says is premature, and on a push to `main`
checks the manifest against the live row.

The top-level `version:` in a Project's `navigator.yaml` selects its exact gate release. An installed CLI from another
release or a development build runs the pinned executable before applying any content rule or fix, forwarding flags,
output, and exit status. It verifies and caches a missing release. A failed download or version check stops the run.
`navigator validate` uses the installed CLI's rules.

In a Project repository the gate is the required check. Each repository carries two thin callers of Navigator's reusable
workflow, pinned to the release in `navigator.yaml`; the public sample snapshots (`neon-law/sample-*`) show them in
full:

```yaml
# .github/workflows/ci.yml — every pull request
on:
  pull_request:
permissions:
  contents: read
  id-token: write
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@YY.M.D
```

```yaml
# .github/workflows/cd.yml — every merge to main re-runs the gate with live checks
on:
  push:
    branches: [main]
jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@YY.M.D
```

`navigator validate [DIR]` is the same rule set aimed at an arbitrary directory. It defaults to `.`, takes `--fix`,
`--errors-only`, and `--ci`, and makes no assumption about the surrounding repository.

- Reference: [`gate`](../gate.md)
