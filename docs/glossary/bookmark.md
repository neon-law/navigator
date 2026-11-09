---
title: "Bookmark"
description: "Jujutsu's name for a Git branch: the label the forge reads, which stays on its change until moved."
---

A Bookmark is how Jujutsu (`jj`) names a Git branch in a [Project](project.md) checkout that `navigator project
repository jj` has colocated. The [Forge](forge.md) sees each pushed Bookmark as an ordinary branch, so a review request
opens from it the same way it opens from a Git branch.

A Bookmark does not advance when a new change is made on top of it; it moves only with `jj bookmark set` (or `move`).
`jj git push --change @` sidesteps that by creating a Bookmark for the current change, named `push/<change-id>` by the
repo setting the command writes — the same `push/` namespace `navigator project repository push` branches into.
