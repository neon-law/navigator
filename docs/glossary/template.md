---
title: "Template"
description: >-
  A Template is the authored Markdown file that supplies a Notation’s metadata, questionnaire, workflow, and body.
---

The authored Markdown file that a Notation is created from. In a Project repository it lives under `drafts/`, the
repository folder (`templates/` is the alias spelling); Navigator's shared catalog keeps it under `templates/`. It
contains the firm's drafted text and the machine that gathers what the text needs. It has four parts: **metadata**,
**questionnaire**, **workflow**, and **body**. A lawyer calls the same file a **[Draft](draft.md)**; the two nouns name
one thing.

**Metadata is a conceptual grouping, not a literal nested YAML key.** It names the frontmatter keys that classify the
file — `kind:`, jurisdiction, respondent — which sit at the top level of the frontmatter block. The filename stem is the
Template's stable code. A mismatched legacy `code:` warns during the one-release compatibility window and is removed by
`navigator validate --fix`; later releases reject any remaining `code:` key. A template that declared an actual
`metadata:` mapping would be malformed. The full anatomy is documented in [`notation`](../notation.md).

A Template is versioned by append rather than by edit: `templates.is_current` marks the live revision and a change adds
a row. Every Notation pins the exact `templates` row it was created from, which is what keeps approved text from
silently re-rendering out of a later revision.

Each row carries `body_sha256`, the SHA-256 of its body. A row synced from a Project repository also carries
`source_commit_sha`, the commit it was read from; the store refuses a project-scoped row without both. A shared-catalog
row carries no commit. Opening a Notation copies both values onto it, and the renderer refuses a body whose bytes do not
match the copied digest.
