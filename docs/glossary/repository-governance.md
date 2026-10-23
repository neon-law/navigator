---
title: "Repository Governance"
description: >-
  Repository Governance is the merge policy every Project repository on Forgejo keeps: squash merge only, a protected
  main, and the project gate as its required check.
---

The settings a [Project](project.md)'s Forgejo repository holds so that every change reaches `main` by pull request. It
allows squash merge only and deletes the branch after a merge. It protects `main`: no direct push, no exemption for
administrators, signed commits required, and the [Gate](gate.md) check `gate / gate (pull_request)` required to pass. No
approving review is required, but a rejected review or an unresolved conversation blocks the merge.

`cloud::forgejo_governance` holds the one copy, in [`forgejo_governance.rs`](../../cloud/src/forgejo_governance.rs).
`reconcile` converges a repository and reads it back. `schedule` checks the governance again before it asks Forgejo to
squash-merge a pull request once its checks pass. If the required check has already passed, it merges right away. The
operator runs it through `navigator ops forgejo setup|auto-merge|schedule-open`. The [Nightly Project
Update](nightly-project-update.md) reconciles every open Project's repository each night and counts the repositories it
repaired.
