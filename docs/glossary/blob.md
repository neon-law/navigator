---
title: "Blob"
description: "A Blob is the committed YAML pointer in a Project repository that names one Document's live revision."
---

The committed pointer for one [Document](document.md) in a [Project](project.md) repository: `blobs/<slug>.yaml`. It
names the Document's live revision — `kind`, `visibility`, the revision's `sha256`, size, and `created_at`, and any
`signing`, `notarization`, or `service` record — and never holds an object-storage coordinate or the bytes themselves.
The bytes it names sit at `documents/<slug>`, which the root `.gitignore` keeps out of Git; `navigator project sync`
uploads them from there or pulls them down to there. The slug is the same path in both folders and keeps the document's
extension, so `blobs/pleadings/motion.pdf.yaml` names `documents/pleadings/motion.pdf`. `.yaml` is written; the retired
`.yml` spelling is still read.

A Blob is reviewed in a pull request like any other source: changing its `visibility` to `client` is a client-release
decision. Rule `Y003` validates every Blob, and `Y014` refuses a pointer left under `documents/`. The object-storage key
prefix `blobs/<sha>` for bare content [Assets](asset.md) is a different thing that shares the word.

- Shape: [`store::document_pointers::DocumentPointer`](../../store/src/document_pointers.rs) · Contract:
  [Document staging and pointers](../project-repositories.md#document-staging-and-pointers)
