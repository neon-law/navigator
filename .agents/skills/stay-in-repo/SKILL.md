---
name: stay-in-repo
description: >-
  Keep Project repository work inside its own checkout. Use at session start and before accessing files outside the
  tree.
---

# Stay inside this repository

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

Read, search, and edit only files inside this repository's own checkout. Do not open, read, or search files elsewhere on
the machine: not the home directory, a sibling checkout, another Project's repository, system paths, or another worktree
of this repository.

This is a scope rule, not a security boundary. Another folder on this machine may hold a real client matter, a different
Project's deliberately different fixture data, or a different revision of the Navigator route this bundle mounts under.
Copying from it imports a fact without its reason, and the next contributor cannot verify a change justified by a file
they cannot see.

If a task cannot be done without something outside this tree, say so and ask. Installing dependencies and fetching
public sources (a package registry, a published release, public case law) is fine.

## Exceptions

- **`node_modules/` inside this tree.** Reading a dependency's shipped types or stylesheet is part of using the
  toolchain.
- **The current session's own scratchpad.** Write temporary scripts, working notes, and drafts another skill keeps out
  of the repository (an implementation audit, for instance) there, and read them back from there. This opens only that
  one directory: other sessions' scratch directories, sibling checkouts, the home directory, and system paths stay off
  limits.

Never leave a scratch file in this repository's working tree; it becomes somebody's confusing diff.
