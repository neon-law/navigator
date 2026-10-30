---
name: push-changes
description: >
  Rebase, gate, sign, and push a Project repository change through a ready pull request against main, then wait for
  merge. Trigger on "push changes", "push this", "ship it", or "land it". Synced verbatim into every Project
  repository's `.agents/skills/`; this canonical copy lives in Navigator's own `.agents/skills/`.
---

# Push changes

## Find the term first

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

## The rule

"Push changes" means finish the pull request through merge. The job ends when the pull request is merged and local
`main` is current.

1. Inspect `git status --short --branch` and review the diff. Before committing, attach work on `main` or a detached
   HEAD to a fresh topic branch with `git switch -c <branch>`; this retains local commits and uncommitted work. Reuse an
   existing topic branch when continuing its pull request. Stage only the requested source and document pointers; keep
   document bytes, notes, credentials, and client data out of Git. Run `navigator project gate`, then make a signed
   commit with `git commit -S`. Commit work in progress before rebasing; never stash or commit on `main`.
2. Fetch `origin/main`, then run `git rebase -S origin/main` on the topic branch. Never push to `main`. Follow
   [pull-changes](../pull-changes/SKILL.md) for conflict handling and squash-merged duplicates, keeping rebased commits
   signed. If a conflict cannot be resolved safely, abort the rebase and report it.
3. Run `navigator project gate` again, then `git push --force-with-lease -u origin <branch>`. Never use plain
   `--force`. Reuse the same branch and existing pull request when continuing an earlier attempt.
4. Open a pull request against `main`, ready for review. Inspect the origin URL to choose the forge. On GitHub, use
   `gh pr create --base main` and inspect `gh pr view <number> --json autoMergeRequest,state`. Respect App-owned
   automatic squash auto-merge; confirm it is armed or the state is already `MERGED`. If automation does not arm it,
   inspect the failure and report a policy or permission blocker instead of merging manually. On another forge, use its
   equivalent ready-for-review and merge-when-checks-pass controls and verify the pull request's merge policy.
5. Wait for the required checks and the actual merge. On GitHub, use `gh pr checks <number> --watch`, then poll
   `gh pr view <number> --json state,mergeCommit` until `state` is `MERGED`; green checks alone do not finish the job.
   On another forge, watch its required checks and verify its merged state. Fix failed checks on this branch. If main
   moves and conflicts, fetch, signed-rebase, gate, and push this branch again. Report an unresolved review, permission,
   or policy blocker honestly; do not call an open or closed-unmerged pull request complete.
6. After merge, fetch origin. Run `git switch main` and `git pull --rebase origin main` when local main is available.
   If main belongs to another worktree, keep this checkout isolated and fast-forward it to the fetched `origin/main`
   with `git switch --detach origin/main`; report that the other worktree's main was not updated. Never reset a dirty
   tree or change another worktree. Report the pull request URL and merged commit.
