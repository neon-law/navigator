---
title: "Notation Import"
description: >-
  Carrying an edited Word document back into the notation Template it was rendered from.
---

The second half of `navigator notation word`: an edited `.docx` carries the export's provenance record (template path,
template hash), and `navigator notation import` verifies the hash against the current [Template](template.md), refuses
drafts still carrying tracked changes or review comments, refuses any edit that drops a `{{placeholder}}` or `{{>
partial}}` the template needs or invents one it never had, and splices the remaining edits into the template's Markdown.
Rendered tables are carried verbatim and never touched. `--dry-run` prints the plan and the updated template without
writing anything.

The verb lives in the `word` crate's import planner (`word/src/import.rs`) and the CLI's `NotationCmd::Import`.
