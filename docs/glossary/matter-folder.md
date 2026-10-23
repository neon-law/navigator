---
title: "Matter Folder"
description: "A Matter Folder is one of five lawyer-facing folders in a Project checkout that hold git-ignored bytes."
---

One of the five folders `navigator project sync` creates under a Project checkout's `documents/`: `Client Production`,
`Correspondence (Served)`, `Discovery`, `Docket Filings`, and `Work Product`. A lawyer browses them in Finder the way a
case-management system groups a matter's papers. Each holds [Document](document.md) bytes only, and Git ignores them.

A document's pointer is its [Blob](blob.md) at `blobs/<slug>.yaml`, never in a Matter Folder. Its optional `folder:` key
names the Matter Folder. Without one, the folder follows from `kind`: `exhibit` → Client Production, `letter` →
Correspondence (Served), `pleading` and `filing` → Docket Filings, everything else → Work Product. A subfolder inside a
Matter Folder is part of the [Document Identity](document-identity.md) slug; the Matter Folder itself is not.

- Vocabulary: [`store::document_pointers::MatterFolder`](../../store/src/document_pointers.rs) · Layout:
  [`cli/src/document_layout.rs`](../../cli/src/document_layout.rs)
