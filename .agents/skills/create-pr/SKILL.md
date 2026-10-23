---
name: create-pr
description: >
  Turn a dirty working tree into a clean pull request against `main`: survey every change, group the files into
  Conventional Commits by blast radius (one concern per commit), run the gate, branch off `main`, capture a visual for
  any user-visible change, push, and open the PR ready for review, not as a draft. Trigger when the user says
  "/create-pr", "create a PR", "open a pull request", "commit and PR these changes", "group these into commits and ship
  them", or has a dirty working tree they want landed, and as the ship step of
  [`implement-issue`](../implement-issue/SKILL.md). Stops at "PR open"; building images and deploying to prod is a
  separate flow.
---

# create-pr

Before preparing a public PR, read [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).
The branch, commits, PR body, capture, and every linked planning surface use only firm-owned or synthetic content.
Client data, legal files, real contact details, and production identifiers never leave Navigator-managed systems.

Read, in order:

- **[docs/agent-workflows.md → Create a PR](../../../docs/agent-workflows.md#create-a-pr)**: the whole flow (survey,
  group into commits with the Conventional Commit type table, run the gate, branch, commit, capture a visual, push, open
  the PR), plus the `gh auth` recipe for an unauthenticated shell.
- **[docs/gitops.md](../../../docs/gitops.md)**: branch → PR → auto-merge. `main` is squash-merge-only, and CI arms
  auto-merge when the PR opens.

Rules:

- Start every change in a Codex or Claude **New Worktree**, then run `navigator dev worktree-env up --branch <topic>`
  once. The CLI names the linked worktree's PR branch in place; it creates a sibling only when started from the primary
  checkout outside the app workflow.
- Run the content gate, then open the PR. Do not re-run the workspace tests, the `features` harness, or coverage here;
  they ran while the change was written, and CI runs them again. Use the CLI crate when `navigator` is not on `PATH`:

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
- **If the change removes anything, sweep before pushing.** `git grep -n '<removed-name>'` returns nothing apart from
  the test that guards its absence. Manifest entries, fixtures, and doc prose hide half-removals; see [[rust]].
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
  transition). Videos and animated walkthroughs must be screen recordings of the actual running Navigator website and
  its real interaction states. Do not use AI-generated video, motion graphics, mockups, or synthetic animations as
  product footage.
- For authenticated screenshots, follow the worktree login flow in [[open-admin-server]]: grant lawyer against the
  same store as `web`, then sign in through Rauthy for a real session cookie.
- **Audit the teaching surfaces (advisory).** Before pushing, run [[author-docs]] against what this branch changed.
  Fix confirmed drift or escalate per its routing, updating the surface that owns each changed fact in the same commits.
- **Auto-merge must be on.** Push, run `gh pr create --base main` with no `--draft`, then confirm it is armed:
  `gh pr view --json autoMergeRequest`. If it is `null`, arm it (`gh pr merge --auto --squash`, or the app's
  `set_auto_merge`) and report the PR URL. Open it ready for review, not as a draft: auto-merge is armed only on a
  non-draft open. Use `--draft` only when the user asks to hold the PR. The PR lands once the gate is green and its
  review threads resolve.
