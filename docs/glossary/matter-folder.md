---
title: "Matter Folder"
description: "A Matter Folder is one of the lawyer-facing folders in a Project checkout that hold git-ignored bytes."
---

One of the folders `navigator project sync` creates under a Project checkout's `documents/`: `Client Production`,
`Correspondence (Served)`, `Discovery (Served)`, `Docket Filings`, `Court Rules`, `Engagement`, `Work Product`, and the
seven fixed subfolders below `Work Product`, each suffixed so it reads unambiguously out of context — `Client Calls
(Work Product)`, `Correspondence (Work Product)`, `Discovery (Work Product)`, `Motions (Work Product)`, `Pleadings (Work
Product)`, `Research (Work Product)`, and `Stipulations (Work Product)` (LAW-215). A lawyer browses them in Finder the
way a case-management system groups a matter's papers. Each holds [Document](document.md) [Blobs](blob.md) only, and Git
ignores them.

The `(Work Product)` suffix separates the firm's drafts from what was actually served (`Correspondence (Work Product)`
against `Correspondence (Served)`), and a downloaded or attached file reads unambiguously out of context. The bare
`Discovery` folder a five-folder checkout holds predates LAW-215: a fresh checkout never creates it, and `navigator
project repository migrate-folders` refiles it onto `Discovery (Served)`. For a document created in the checkout,
lawyer-made subfolders inside a Matter Folder (one per motion, `Evidence Summaries/`, `Answer/`) stay part of the
[Document Identity](document-identity.md) slug; the fixed `(Work Product)` names do not. Recursive Drive imports use the
separate placement described below.

A document's [Pointer](pointer.md) lives at `blobs/<slug>.yaml`, never in a Matter Folder. Its optional `folder:` key
names the Matter Folder — a `Work Product` subfolder's name is its whole path below `documents/`. Without one, the
folder follows from `kind`: `exhibit` → Client Production, `letter` → Correspondence (Served), `pleading`, `filing`, and
`order` → [Docket Filings](docket-filings.md), `memo` → Research (Work Product), `transcript` → Client Calls (Work
Product), `onboarding` → Engagement, everything else → Work Product.

The Project page shows the same layout. A lawyer sees every Matter Folder, empty ones included, with each document at
its path below `documents/`. A client sees only the documents shared with them, each once as its newest shared revision,
under only the folders that hold one. The page reads the folder from `kind`, because a pointer's `folder:` lives in the
repository.

- Vocabulary: [`store::document_pointers::MatterFolder`](../../store/src/document_pointers.rs) · Layout:
  [`cli/src/document_layout.rs`](../../cli/src/document_layout.rs)

Recursive Drive imports keep placement separate from Document Identity. A Pointer's optional `checkout_path:` names the
complete safe path below `documents/`; it already contains its Matter Folder prefix, so the CLI adds none. Internal tree
paths retain canonical prefixes once. Client Submissions is an external intake channel, not another Matter Folder: its
files are placed under Client Production with their nested subpaths preserved. The original provider root ID and
relative path remain Asset provenance. Moving a provider file changes placement, not its slug.
