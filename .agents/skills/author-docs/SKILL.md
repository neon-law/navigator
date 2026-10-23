---
name: author-docs
description: Audit teaching surfaces changed by a branch and write one citation-backed advisory report in `/tmp`.
---

# Author docs

Read [`docs/agent-workflows.md`](../../../docs/agent-workflows.md) and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md) first. Code is the source of truth;
docs, comments, tests, workshops, and skills follow it.

1. Confirm this is a non-primary worktree and compare the branch with its local `origin/main`.
2. Read the changed teaching surfaces and the source or test they describe.
3. Classify every finding as confirmed drift, ambiguity, or not drift; cite both sides as `path:line`. Past-tense
   narration ("we used to", "previously", "no longer", incident backstory) is confirmed drift.
4. Write one report at `/tmp/navigator-author-docs/<branch>-<base>.md`, even when there are zero findings.
5. Keep citations in the report, not reader-facing prose. Do not change files during this audit.

Convene the smallest matching council for an unresolved architecture, client-facing, or legal-copy decision. Do not copy
client data, legal files, real contact details, or production identifiers into the report.
