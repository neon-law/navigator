---
title: "Docket Filings"
description: "Docket Filings is the Matter Folder for court papers and documents filed through docket ingest."
---

The [Matter Folder](matter-folder.md) at `documents/Docket Filings/` for court papers. [Documents](document.md) with
`kind: pleading`, `filing`, or `order` land here by default; an explicit `folder: Docket Filings` selects it for another
kind. Docket ingest records this folder when filing a downloaded document.

This folder holds Git-ignored bytes. Each document's tracked [Pointer](pointer.md) lives at `blobs/<slug>.yaml`, where
the slug is the path below the Matter Folder. A [Docket Entry](docket-entry.md) links a court record to its Document;
the [Case Timeline](case-timeline.md) presents those records with the Project's open Deadlines.

- Folder vocabulary: [`store::document_pointers::MatterFolder`](../../store/src/document_pointers.rs) · Layout:
  [`cli/src/document_layout.rs`](../../cli/src/document_layout.rs)
