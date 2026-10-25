---
title: "Pointer"
description: "A Pointer is the tracked YAML file that names one Document's current revision and never contains bytes."
---

The tracked YAML file in a [Project](project.md) repository that names one [Document Identity](document-identity.md)'s
current revision: `blobs/<slug>.yaml`. It records the revision's asset id, `sha256`, size, `kind`, `visibility`, and
`created_at`, and any `signing`, `notarization`, or `service` record. A Pointer never contains bytes and never holds an
object-storage coordinate. The [Blob](blob.md) it names sits in the [Matter Folder](matter-folder.md) selected by its
`folder:` or `kind`, under the Git-ignored `documents/`. The slug keeps the document's extension and its path below that
Matter Folder. With `kind: pleading`, `blobs/motion.pdf.yaml` names `documents/Docket Filings/motion.pdf`.

`navigator project sync` uploads or downloads those bytes. Navigator writes `.yaml` and also reads `.yml`.

A Pointer is reviewed in a pull request like any other source: changing its `visibility` to `client` is a client-release
decision. Rule `Y003` validates every Pointer, and `Y014` refuses one left under `documents/`. The folder is named
`blobs/` after what its files point to; its files are Pointers, not Blobs.

- Shape: [`store::document_pointers::DocumentPointer`](../../store/src/document_pointers.rs) · Contract:
  [Document staging and pointers](../project-repositories.md#document-staging-and-pointers)
