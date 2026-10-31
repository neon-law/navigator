---
name: progress
description: >
  Triage Linear against shipped code and open pull requests. Close an issue only when that code makes completion
  certain, comment when it does not, implement up to five ready issues as separate commits, and open one PR. The turn
  ends when that PR's required CI is green.
---

# `/progress`: move a project forward

Use [`triage-projects`](../triage-projects/SKILL.md) for evidence and issue decisions, then continue into implementation
instead of stopping at its handoff. Follow [`docs/agent-workflows.md`](../../../docs/agent-workflows.md),
[`implement-issue`](../implement-issue/SKILL.md), and [`create-pr`](../create-pr/SKILL.md) for grounding,
implementation, and shipping. Keep client data out of the repository and fixtures.

## Triage the project

1. Start in a New Worktree with the `create-pr` worktree and environment setup. Refresh `origin/main`; preserve
   unrelated changes.
2. Run the [`triage-projects`](../triage-projects/SKILL.md) audit: read `docs/public-contributor-safety.md`,
   `docs/glossary/`, relevant docs, and complete Linear conversations. Reconcile project updates and issue states with
   shipped code, tests, merged PRs, open PRs, and active worktrees. Adjudicate each issue as still valid, shipped,
   duplicate/superseded, or blocked. `triage-projects` applies conclusive tracker cleanup; this pass adds uncertainty
   comments and continues into implementation.
3. Read every open pull request before changing a Linear issue. Record its repository, number, changed paths, and
   linked issues; do the same for active worktrees. An open pull request is in-flight work: leave its linked issues
   open, exclude them from this PR, and keep their paths out of the change. Shipped code is the tree on refreshed
   `origin/main`.
4. Close an issue only when you can point at code and tests on that tree that satisfy it. Mark it completed and comment
   with those citations. When any needed fact is missing or ambiguous, leave it open and comment with what you
   inspected, what the code and open pull requests show, and what remains unconfirmed. Comment on every uncertain issue
   in the triage set. Close a duplicate or superseded issue only under the same certainty, naming the surviving issue.
5. Select 1–5 ready issues that can safely share one PR, one issue per commit. If none is ready, report the findings,
   the issues closed, the issues commented on, and the blockers, and skip the PR. If no compatible set fits one PR,
   report the split and leave the rest for separate passes.

## Implement and update

For every selected issue, in the same worktree:

1. Re-ground its full issue and comment history with [`implement-issue`](../implement-issue/SKILL.md) and verify it
   against the refreshed baseline. An issue already satisfied on `origin/main` closes under the triage rule and stays
   out of this PR. So does a duplicate, superseded, or blocked issue; close it only under the same certainty, otherwise
   comment.
2. State the smallest behavior, covering test, and exact files before editing. Use TDD and the relevant repository
   skills. Stay within the issue's acceptance criteria. Verify with focused tests and, when applicable, the documented
   UI workflow.
3. Commit that issue's implementation and proof before starting the next. Stage explicit paths and use a Conventional
   Commit subject. Keep issue titles and project names out of commits and all public surfaces.

Update Linear project progress and triage fields only from verified findings: what shipped, what remains blocked, which
issues closed from shipped code, which received an uncertainty comment, and which this PR addresses. Keep issue titles,
project or initiative names, and Linear URLs out of public material. A Linear comment may cite paths and tests; it
follows [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

Implemented issues stay open until merge. List their bare identifiers, with the right team prefix, in one PR body
trailer:

```text
Closes ENG-1234, ENG-1235
```

## Open one PR

After all selected commits, use [`create-pr`](../create-pr/SKILL.md) for the final rebase, teaching-surface audit,
content gate, and PR creation. Open one non-draft PR against `main` with one `Closes` trailer listing every selected
issue. Keep unrelated changes out of the branch. Leave auto-merge and the merge itself to CI.

Then follow the PR until the required checks on its head are green. When a check fails, fix it within the selected
issues, amend the corresponding issue commit where practical (otherwise add a focused follow-up commit), push, and wait
again. When the required checks are green, stop. Report the PR link, green checks, issue identifiers, commit subjects,
test evidence, issues closed from shipped code, issues that received an uncertainty comment, and any blocker that kept
CI from going green. When Linear or GitHub is unavailable, finish the repository work and name the blocked external
action.
