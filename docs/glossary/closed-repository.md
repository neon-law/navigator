---
title: "Closed Repository"
description: >-
  A Closed Repository is a ZIP archive of a Project's final working tree, filed when the repository is no longer needed.
---

The [Asset](asset.md) kind `rules::kind::Kind::ClosedRepository` (`kind: closed_repository`) names: a zip of a closed
[Project](project.md)'s repository working tree at its final commit, with no git history, filed on the matter once the
repository is redundant. It follows the [Offboarding](../../templates/notations/neon_law/offboarding__letter.md) close
rather than gating it — a matter closes on its own signed closing letter, and `navigator project close <code>` files the
archive immediately after, from the local checkout it is run in. Asset-lane only: `Kind::valid_for(Lane::Template)`
refuses it, so no template ever declares this kind.

Deleting the repository from its forge is a separate, deliberate step an operator (or `delete_closed_repository`) takes
only after this document exists and its recorded commit SHA is checked against the live repository's current HEAD. A
Closed Repository is a working-tree snapshot, not the recovery record: deletion also waits until a committed [Project
Recovery Archive](project-recovery-archive.md) cut after the close holds a verified Git bundle with that HEAD, every
required [Handoff Bundle](handoff-bundle.md) is acknowledged, and no [Legal Hold](legal-hold.md) stands on the matter.
