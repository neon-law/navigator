---
title: "Repository"
description: "A Repository is a provenance record for an external Git source used by a Notation."
---

A provenance record for an external Git repository that notation content came from. The `git_repository` row holds a
hash of the remote URL and the last imported commit SHA. One row per external source, shared across Projects. Rows are
written by the canonical seed in [`store::seed`](../../store/src/seed.rs); no command fetches these remotes today.

`navigator site seed` walks a local directory given on the command line without recording an external source here. The
provenance row tracks outside sources from which Navigator imports notation content. A [Project](project.md) records its
own source repository separately through `project.repository_url`.

- Schema and queries: [`store::git_repositories`](../../store/src/git_repositories.rs) —
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)

The table is `git_repository`.

Schema: [`git_repository`](../../store/src/schema/navigator.surql#L1259).
