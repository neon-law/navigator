---
name: implement-issue
description: >
  Ground and implement one Linear issue in its own Navigator worktree, then open a ready pull request. Use when the user
  asks to implement, pick up, start, or build ENG-NN, whether or not it has already been triaged. Refresh from
  origin/main, read the full issue, verify it against shipped code and tests, adjudicate it, make the smallest
  test-driven change, and open the PR ready for review. Do not use for portfolio triage or a plan-only request.
---

# `/implement-issue`: ground and implement one issue

Turn one issue identifier into one grounded, minimal, proven implementation and one ready pull request in one session.
Use [`triage-issue`](../triage-issue/SKILL.md) only when the user explicitly wants a Linear plan without code. Follow
[`create-pr`](../create-pr/SKILL.md) for grouping, gates, the Linear trailer, and opening the PR ready for review, not
as a draft.

## Start current

Before editing, confirm that `pwd -P` is a non-primary entry in `git worktree list --porcelain`. Preserve unrelated
changes. Fetch and signed-rebase on the shipped baseline:

```bash
rtk git fetch origin main
git rebase -S origin/main
```

Do not implement from a stale local `main`, and do not merge `origin/main`. If the worktree is dirty or the rebase
cannot complete safely, stop and report rather than combining unrelated work.

## Ground the issue

Read `docs/public-contributor-safety.md`, `docs/glossary/`, and the narrowest relevant source of truth from
`docs/index.md`. Fetch the Linear issue with `get_issue` using `includeRelations: true`, then `list_comments`, and read
from the opening body through the last comment. Treat an existing triage comment as evidence, not authority: refresh
every claim against `origin/main`.

Determine whether the issue already shipped: check Linear's GitHub linkage, then correlated merged pull requests, then
the source. Derive the repository from `origin`, list its merged pull requests, and call Linear's `get_diff` for each
candidate GitHub URL. A returned issue is the canonical association. If no diff resolves, look for an issue reference in
the pull-request body and corroborate branch-name or title matches with explicit Linear evidence. Record when only a
heuristic is available.

A pull-request search proves whether an issue was linked to merged work, not that the capability is absent. Search the
relevant code, configuration, public API, and tests at `origin/main` before calling it missing. For proposed new
structure, also follow the source checks in [`author-linear-issue`](../author-linear-issue/SKILL.md).

Reproduce the current behavior where practical. If an unknown still blocks a grounded scope, run the smallest throwaway
Rust spike that answers it and record the command, observation, and conclusion. Inspect active worktrees and open pull
requests for overlapping files.

## Adjudicate before editing

Reach exactly one verdict:

- **Still valid:** name the smallest behavior, covering test, exact blast-radius files, and collisions, then continue.
- **Already shipped:** cite the correlated merge and source or test evidence, then stop without editing.
- **Duplicate or superseded:** name the surviving issue and evidence, then stop without editing.
- **Blocked on a decision or dependency:** name the blocker, owner, and smallest next action, then stop without editing.

Do not post a separate triage-plan comment; the verdict gates implementation within this session. Do not infer a missing
legal, product, or operator decision.

## Implement with TDD

Name the smallest behavior and the test that proves it. Add or adjust the covering test first and watch it fail for the
intended reason. Implement only enough to pass it, then run the focused test and the applicable repository gate.

Use the workspace's established seams and specialized skills. When the issue changes a route, authorization boundary,
workflow, or user-visible behavior, test that surface, not an incidental helper.

Document the present system only: current behavior, contracts, and invariants. Remove superseded code and instructions
in the same change. Do not add decision history, compatibility narration, or "used to" prose.

## Verify and open the PR

Run the focused test that covers the change. Verify user-facing changes through the documented browser loop.

For agent-facing Rust output, use `rtk cargo build`, `check`, `clippy`, `test`, or `nextest` when RTK is installed. It
compresses repetitive output but does not speed compilation. Keep formatting, coverage, project gates, machine-readable
output, and raw diagnostics on plain Cargo. Use `rtk git`/`rtk gh` for human-facing summaries; keep raw commands for
revision values, rebases, API/JSON, patches, auth tokens, and exact body/URL operations.

Before opening the PR, rebase with `git rebase -S origin/main` again and run `navigator validate` (or `cargo run -p cli
--quiet -- validate` when `navigator` is not on `PATH`). Do not re-run the workspace tests, the `features` harness, or
coverage; CI runs them. Keep the issue identifier out of public prose except the bare identifier permitted by
`docs/agent-workflows.md`. Then follow [`create-pr`](../create-pr/SKILL.md): push and open a pull request against
`main`, ready for review, not as a draft. Auto-merge is armed only on a non-draft open. Do not stop at a green worktree,
and do not open a draft unless the user asked to hold the change.
