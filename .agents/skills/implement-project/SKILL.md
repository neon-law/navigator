---
name: implement-project
description: >
  Implement every open Linear issue in one Engineering project, one per commit, then open a ready pull request. Use when
  asked to implement a Linear project or all issues in a project. Reconcile project and issue state with shipped code
  first; do not duplicate already merged work.
---

# `/implement-project`: complete one Linear project

Implement the project's remaining valid issues in one task and one pull request. Use
[`implement-issue`](../implement-issue/SKILL.md) as the per-issue implementation contract, and
[`create-pr`](../create-pr/SKILL.md) for the final branch, gate, and pull request.

## Establish the project scope

1. Read [`docs/agent-workflows.md`](../../../docs/agent-workflows.md),
   [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md), and this repo's `AGENTS.md`.
2. Start in the task's New Worktree. Confirm its physical path is a non-primary entry in
   `git worktree list --porcelain`; do not create a nested worktree. Prepare the branch and environment once as
   described in `create-pr`.
3. Fetch `origin/main`, rebase the clean worktree with `git rebase -S origin/main`, and record its baseline. Stop if
   user changes, conflicts, or inaccessible Git metadata prevent a safe start.
4. Fetch the Linear project by identifier or URL, then list every issue in it, including completed, canceled, and
   archived issues when available. Fetch each open issue in full with relations and comments. Treat project health,
   issue state, attachments, and closing trailers as clues to reconcile, not proof of implementation.
5. For each issue, inspect Linear's GitHub linkage and exact diff lookup for candidate pull requests. Verify merged
   changes against `origin/main` source and tests. Mark each issue as already shipped, duplicate/superseded, blocked on
   a specific decision or dependency, or still valid. Do not redo shipped work or silently choose product semantics.
6. Check active worktrees and open pull requests for file collisions. Order remaining issues by dependencies and project
   relations. If an open PR already implements an issue, exclude it from this change and report that evidence.

## Implement the remaining issues

For every still-valid issue, in dependency order:

1. Follow all grounding and adjudication steps in `implement-issue`: glossary, narrow source docs, relevant code,
   tests, the complete issue conversation, and a stated minimal behavior and proof.
2. Implement with test-first development, using the repository's focused test and applicable UI verification loop.
   Apply specialized skills for changed seams. Preserve the public-contributor safety boundary.
3. Commit that issue's complete implementation and tests before starting the next issue. Stage explicit paths and use a
   Conventional Commit subject. Keep commits separate even when issues share files; later commits may build on earlier
   ones. Reference only the bare issue identifier in public commit text where needed.
4. Recheck dependent issues against the new source after each commit. If an issue is already satisfied by an earlier
   commit in this task, record the evidence and avoid a redundant commit.

If all issues are already shipped, stop without an empty code PR and report the tracker drift. If an issue depends on an
unresolved product, legal, or operator decision, finish independent issues, then report the blocker and its owner or
smallest decision needed. Do not claim the whole project complete while required issues remain blocked.

## Verify and open one pull request

After the last valid issue commit, survey the full diff and commit list. Run focused tests as each issue is implemented,
the required user-visible verification, teaching-surface advisory, and the final `navigator validate` gate. Do not rerun
the workspace suite at PR preparation. Rebase on current `origin/main`, resolve only changes grounded in the issue
acceptance, and verify the commits remain one per issue.

Follow `create-pr` for screenshots of user-visible behavior, safe public prose, push, and one non-draft pull request
against `main`. Include one closing trailer per issue actually completed by that pull request; use a non-closing
reference for work that remains blocked or pending. Confirm auto-merge is armed. Attach the pull request to the task and
report its URL, issue-to-commit mapping, verification, shipped issues excluded, and any unresolved blockers.

Never publish Linear project or issue titles, project names, or Linear URLs in branch names, commits, screenshots, or
pull request prose. Keep roadmap details in Linear and identify issues publicly by their bare identifiers only.
