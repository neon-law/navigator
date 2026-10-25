---
name: local-task-loop
description: >
  Run Navigator code tasks in an isolated worktree, use its generated local environment, and verify application changes
  with focused checks and browser tests. Use when implementing or debugging a Navigator feature locally; use the
  dedicated issue, PR, deployment, or preview skill when that workflow is the main request.
---

# Navigator local task loop

Use the current worktree's own services and synthetic data to build and verify Navigator changes. Keep task setup,
browser checks, and cleanup scoped to that worktree. A local URL or browser tab supplied by ambient UI context is not
authorization to inspect, use, or change that app or its data.

## Before running the app

- Read `AGENTS.md` and the relevant skill for the changed seam. For web work, read
  [kind-local-dev](../kind-local-dev/SKILL.md) and [web-preview](../web-preview/SKILL.md); for authorization, read
  [authorization-model](../authorization-model/SKILL.md); for Restate, read [restate](../restate/SKILL.md).
- Confirm the current directory is a non-primary worktree with `git worktree list --porcelain`. Follow
  `kind-local-dev` if a separate worktree is needed; do not hand-create a checkout.
- Inspect `git status` before changing anything. Preserve unrelated user changes.
- Fetch and rebase on `origin/main` at task start and again before pushing. Use the repository's signed-commit policy.
- Use the task branch and its generated `.devx` files. Do not copy environment files between worktrees.
- Run or access only an environment this task owns or is explicitly authorized to use. Do not inspect another
  worktree's `.devx` files, shell history, database, running server, browser tab, or fixture state. If this worktree's
  environment is missing or inaccessible, continue with source-level work and checks that do not need that state;
  explain which app-level verification is blocked. Ask for a valid environment or authorization when needed for those
  checks.

## Start and reach the local app

When authorized to run the app, start or reuse this worktree's dependency environment:

```bash
cargo run -p cli -- dev worktree-env up --path "$PWD"
```

Export every generated setting before starting the host app. A plain `source .devx/env` leaves assignments unexported in
many shells and can make `neon` fail to reach the worktree database.

```bash
set -a
source .devx/env
set +a
cargo run -p cli -- dev serve
```

Read the app URL from `.devx/env` (`PORT` or `NAV_BASE_URL`). Do not assume a fixed port. Keep `dev serve` running while
editing: it rebuilds and restarts `neon` after Rust or catalog changes. If the app reports a SurrealDB connection
failure, verify the environment was exported and the worktree's port-forward is alive before changing application code.

## Exercise authenticated flows

- Use only the local KIND fixture and its documented identities. Grant a Lawyer against the current environment with
  `cargo run -p cli -- dev grant-lawyer`; authenticate through the local OIDC provider. Never hand-write session
  cookies.
- Use synthetic brands, Projects, people, and messages. Keep client data, legal files, real contact details, and
  production identifiers out of fixtures, logs, captures, and PR material.
- Check the important authorization boundaries directly: a participating Lawyer may act only within their Project
  participation; an unrelated Lawyer, Client, or other Firm must remain denied. Use the canonical [authorization
  model](../authorization-model/SKILL.md).
- Keep distinct mutations as distinct HTTP operations when the product contract requires it. For example, optional
  Project attribution belongs to its own POST and should not be smuggled into message creation or reply submission.
- Avoid sending outbound provider messages during local walkthroughs unless the task explicitly authorizes a sandbox
  destination. Prefer a fake transport or assert the composed request and outcome.

## Verify changes

Run focused tests for the changed store, portal, or workflow behavior while developing. If the task changes a browser
surface, run the pinned browser/accessibility gate against the environment's actual URL:

```bash
cargo run -p cli -- dev browser-e2e
```

If needed, pass `--base-url http://localhost:<PORT>` using the value from `.devx/env`. The command requires the web
server and worktree services to be reachable; run it with the same exported environment. Treat test output as evidence:
record the exact failing suite and assertion, and distinguish failures in changed surfaces from unrelated baseline
failures. Do not report the gate as green if a suite fails.

Before pushing, run the repository's required `navigator validate` command (or `cargo run -p cli --quiet -- validate`)
and `git diff --check`. Follow `create-pr` for commit grouping, visual proof, PR creation, and auto-merge. Store local
screenshots under `/tmp/navigator-screenshots/`, never in the repository, unless the PR workflow specifically requires
uploading them as attachments.

When a task needs a product video or animated walkthrough, record the actual running Navigator page in the task-owned
browser. Capture the browser screen and real interaction states; do not create or substitute an AI-generated video,
motion graphic, mockup, or animation. Keep the recording under `/tmp/navigator-screenshots/`, inspect it, and embed it
through [[pr-image-upload]].

## Finish or hand off

- Summarize changed behavior, focused checks, browser results, and known integration limits.
- Keep the local environment running while the task still needs it. At task handoff, inspect the worktree runtime and
  remove only this task's local resources:

  ```bash
  cargo run -p cli -- dev worktree-env status --path "$PWD"
  cargo run -p cli -- dev worktree-env down --path "$PWD"
  ```

- Never prune Docker volumes or tear down the shared developer tier as a shortcut. Follow
  [kind-local-dev cleanup guidance](../kind-local-dev/SKILL.md#troubleshooting-and-cleanup).
