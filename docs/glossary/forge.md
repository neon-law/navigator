---
title: "Forge"
description: >-
  A Forge is the service that hosts a Project's source repository and its merge loop — the review request, the
  required check, and the deployment run a merge starts.
---

The Project fleet's repositories live on a forge: GitHub today, Forgejo alongside it (`navigator dev forgejo`), and the
fleet is moving between them. A forge owns the nouns the landing lane reads: the review request that carries a branch's
change to a base, merge-when-green (the forge's own queue for merging a request once its required check passes), the
required check that must be green before the merge, the merge commit a merge produces, and the deployment run the merge
commit starts.

Which forge a repository sits on is a fact of its `origin` remote, not of the code. The verb that lands a change,
`navigator project repository push`, speaks to the [Project](project.md)'s forge through one seam —
`cli/src/projects/forge.rs` — so its report reads the same on either forge, and each forge's quirks stay in that
forge's backend (`cli/src/projects/forge_github.rs` for GitHub).
