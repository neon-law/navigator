---
name: create-pr
description: >-
  Gate and sign repository changes, open a ready PR, verify automatic merge configuration, and address review comments.
---

# create-pr

Before preparing a public PR, read [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).
The branch, commits, PR body, capture, and every linked planning surface use only firm-owned or synthetic content.
Client data, legal files, real contact details, and production identifiers never leave Navigator-managed systems.

Read, in order:

- **[docs/agent-workflows.md → Create a PR](../../../docs/agent-workflows.md#create-a-pr)**: the whole flow (survey,
  group into commits with the Conventional Commit type table, run the gate, branch, commit, capture a visual, push, open
  the PR), plus the `gh auth` recipe for an unauthenticated shell.
- **[docs/gitops.md](../../../docs/gitops.md)**: branch → PR → auto-merge. `main` is squash-merge-only.

Rules:

- Start every change in a Codex or Claude **New Worktree**, then run `navigator dev worktree-env up --branch <topic>`
  once. The CLI names the linked worktree's PR branch in place; it creates a sibling only when started from the primary
  checkout outside the app workflow.
- Run the content gate, then open the PR. Do not re-run the workspace tests, the `features` harness, or coverage here;
  CI runs them. Use the CLI crate when `navigator` is not on `PATH`:

  ```bash
  navigator validate
  ```

  ```bash
  cargo run -p cli --quiet -- validate
  ```

  When RTK is installed, prefix agent-facing `cargo build`, `check`, `clippy`, `test`, and `nextest` with `rtk` to
  collapse repetitive output; failures, warnings, and exit codes stay visible. Keep `cargo fmt`, `navigator validate`,
  `--message-format` consumers, and raw diagnostics on their ordinary commands. Use `rtk git` and `rtk gh` for
  human-facing status, history, diffs, and PR summaries. Keep raw `git` for porcelain, plumbing, revision values,
  rebases, and exact checks; keep raw `gh` for API/JSON, patches, auth tokens, and exact PR body or URL operations.
- Group by blast radius: one reviewable concern per commit, staging each path explicitly.
- **If the change removes anything, sweep before pushing.** `git grep -n '<removed-name>'` returns no obsolete
  references. Test the surviving behavior. Manifest entries, fixtures, and doc prose hide half-removals; see [[rust]].
- **Link the Linear issue by identifier only.** Put one magic-word trailer in the PR body, `Closes ENG-1234`, so Linear
  links the PR and completes the issue on merge. Keep the identifier out of the PR title, which becomes the squash-merge
  subject. The roadmap stays private: no `linear.app` URL (its path carries the title as a slug); no issue title,
  project, initiative, milestone, or cycle name; and no branch name from Linear's **Copy git branch name**, which
  appends that slug. Name the branch `<initials>/eng-1234-<short-neutral-topic>` yourself. See [Linking a PR to its
  Linear issue](../../../docs/agent-workflows.md#linking-a-pr-to-its-linear-issue), including the one exposure this
  cannot close: Linear's own linkback comment.
- Capture a live walkthrough of any user-visible change into `/tmp/navigator-screenshots/`, look at it yourself, and
  embed it in the PR body via [[pr-image-upload]] (one `curl` to the tenant's `user-attachments` store, authenticated by
  `gh auth token`). Use `curl`, not the `gh-image` extension, which cannot target this host. **Default to a GIF of the
  real interaction** ([[web-preview]] §5); use a still only when nothing moves (no keypress, click, or state
  transition). Videos and animations must record the real running Navigator website; never use AI-generated video,
  motion graphics, mockups, or synthetic animations as product footage.
- For authenticated screenshots, follow the worktree login flow in [[open-admin-server]]: grant lawyer against the
  same store as `web`, then sign in through Rauthy for a real session cookie.
- **Audit the teaching surfaces (advisory).** Before pushing, run [[author-docs]] against what this branch changed.
  Fix confirmed drift or escalate per its routing, updating the surface that owns each changed fact in the same commits.
- **Auto-merge is automatic.** Push and run `gh pr create --base main` with no `--draft`. Ready PRs from this
  repository arm App-owned squash auto-merge without a label. Then check `gh pr view --json autoMergeRequest,state`; an
  already-green PR may merge immediately. Report the PR URL and whether it merged or auto-merge is armed. Use `--draft`
  when the user asks to hold the PR. Required checks and resolved review threads still govern merging.
- **Fix CI before reporting.** The PR is not done while any check is red, `CodeQL` included (it counts only new alerts
  in changed code). Fix each failure with [[fix-checks]] and push until every check is green. `CodeQL`
  `rust/cleartext-logging` flags any print of a value parsed from the CLI command enum: drop the interpolation; never
  dismiss the alert.
- **Wait for the review comments, then answer them.** Greptile and human reviewers post after CI starts, and an
  unresolved thread blocks auto-merge like a red check. Address every comment with [[review-pr]] (fix it, or reply with
  the reason not to) and resolve its thread. The PR is not done, or reported done, until every thread is answered.
