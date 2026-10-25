---
name: pull-changes
description: >
  Bring the current branch up to date by rebasing it onto the latest `origin/main`. Trigger whenever the lawyer says
  "pull changes", "pull main", "update the branch", or "sync with main", and before pushing a pull request branch that
  has fallen behind. Never `git pull` into a branch, never merge `origin/main`, never only fetch. Synced verbatim into
  every Project repository's `.agents/skills/`; this canonical copy lives in Navigator's own `.agents/skills/`.
---

# Pull changes

## Find the term first

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

## The rule

"Pull changes" means one thing: rebase the current branch onto the latest `origin/main`. Project repositories keep a
linear history and squash-merge, so a merge commit on a branch is noise, and a bare fetch has not brought anything up to
date.

1. Start from a clean tree: `git status --porcelain` prints nothing. Commit work in progress; never stash.
2. `git fetch origin && git rebase origin/main`. On `main` itself, `git pull --rebase origin main`.
3. On a conflict, resolve it, `git add` the files, and `git rebase --continue`. If it cannot be resolved cleanly,
   `git rebase --abort` and report what conflicted.
4. Run `navigator project gate`.
5. Push a pull request branch with `git push --force-with-lease`, never plain `--force`. Never force-push `main`.

## Squash-merged commits

A branch may still hold commits that were squash-merged into `main`. The rebase drops the duplicates. Where they surface
as conflicts, keep `origin/main`'s version.

## Host-side sync tools

A desktop app's "sync with base branch" tool merges rather than rebases. Use it only when the sandbox blocks a rebase
(for example, a protected file changed on `main`), and say so.
