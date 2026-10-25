---
title: "Blob"
description: "A Blob is the committed YAML pointer in a Project repository that names one Document's live revision."
---

The committed pointer for one [Document](document.md) in a [Project](project.md) repository: `blobs/<slug>.yaml`. It
names the Document's live revision — `kind`, `visibility`, the revision's `sha256`, size, and `created_at`, and any
`signing`, `notarization`, or `service` record — and never holds an object-storage coordinate or the bytes themselves.
The bytes it names sit in the [Matter Folder](matter-folder.md) selected by its `folder:` or `kind`, under the
Git-ignored `documents/`. The slug keeps the document's extension and its path below that Matter Folder:
`blobs/pleadings/motion.pdf.yaml`, with `kind: pleading`, names `documents/Docket Filings/pleadings/motion.pdf`. The
sibling folders `onboarding/`, `offboarding/`, `invoices/`, `cases/`, and `rules/` keep their bytes at
`documents/<slug>`. `navigator project sync` uploads or downloads those bytes. Navigator writes `.yaml` and also reads
`.yml`.

A Blob is reviewed in a pull request like any other source: changing its `visibility` to `client` is a client-release
decision. Rule `Y003` validates every Blob, and `Y014` refuses a pointer left under `documents/`. The object-storage key
prefix `blobs/<sha>` for bare content [Assets](asset.md) is a different thing that shares the word.

- Shape: [`store::document_pointers::DocumentPointer`](../../store/src/document_pointers.rs) · Contract:
  [Document staging and pointers](../project-repositories.md#document-staging-and-pointers)
