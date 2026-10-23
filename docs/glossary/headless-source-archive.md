---
title: "Headless Source Archive"
description: >-
  A Headless Source Archive is a validated ZIP of a Project's source-only files that initializes its Forgejo repository
  as the first commit.
---

The upload that gives a [Project](project.md) its first source commit on Forgejo. It holds only the headless source
roots — the manifest, Drafts, templates, document pointers, seeds, tests, fixtures, and the agent contract — never a
Portal, dependency or build output, a credential, or a legal document. Every file is UTF-8 text.

The CLI and the server apply the same rules (`rules::source_archive`): canonical ASCII paths, no links or special
entries, bounded size and inflation, one wrapper directory stripped, and case or file-and-directory collisions refused.
An invalid archive changes nothing. A valid one is stored as its canonical ZIP, bound to its tree digest, the Project,
its Firm, and an operation key, until the import commits. The first commit carries the digest in a `Source-Tree:`
trailer, so a retry finds an applied import rather than writing twice, and a repository holding other source is never
replaced. Only after the forge reports that commit does the Project record its repository, its provisioning time, and
`git_initialized_at`; the stored archive is then discarded.

A Headless Source Archive is not a [Closed Repository](closed-repository.md), which is a final working-tree ZIP filed as
a document, and it carries no history: moving a repository with its history is a separate migration.

- Rules: [`rules::source_archive`](../../rules/src/source_archive.rs)
- Import: [`store::project_source_imports`](../../store/src/project_source_imports.rs)
