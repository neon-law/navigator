---
title: "Documents Check"
description: "A Documents Check runs a documents tree's matter-folder, filename, and collision
  rules read-only, over any root laid out like documents/."
---

The read-only report `navigator project documents check` prints: the [Matter Folder](matter-folder.md) membership,
invoice-filename, and one-slug rules a Project checkout's `documents/` lives under, run over any tree laid out like
`documents/` — a Google Drive for desktop copy of a matter folder included (LAW-217). Nothing is uploaded, renamed, or
moved; findings exit non-zero so an agent or CI job can act on them, and `--json` emits them machine-readably.

`--documents-dir` points the check at another root; `--drive-local` resolves the Project's matter folder on the local
Drive for desktop mount from the [Project](project.md) code in `navigator.yaml`. A blocked `~/Library/CloudStorage` is
reported as the macOS Files and Folders (or Full Disk Access) setting, never a bare `Operation not permitted`. `--deep`
also compares each committed [Pointer](pointer.md) against the `sha256` and `size_bytes` of the bytes in whichever tree
is being checked.
