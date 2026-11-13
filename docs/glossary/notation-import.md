---
title: "Notation Import"
description: >-
  Carrying an edited Word document back into the notation Template it was rendered from.
---

An edited `.docx` carries the provenance from `hudson notation word`, including the template path, template hash, and
source block hashes. `hudson notation import` verifies the hash against the current [Template](template.md). It refuses
drafts with unresolved tracked changes or review comments and rejects edits that add, remove, or alter a placeholder or
Clause include.

The importer splices valid edits into the template's Markdown. Changed text retains bold and italic runs, and simple
rectangular Word tables become Markdown pipe tables. A stale-template refusal lists changed source blocks for drafts
with a block map. `--dry-run` prints the block-level diff without writing.

The verb lives in the `word` crate's import planner (`word/src/import.rs`) and the CLI's `NotationCmd::Import`.
