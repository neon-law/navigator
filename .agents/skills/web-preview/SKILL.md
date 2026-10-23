---
name: web-preview
description: Run and verify a Navigator page with automatic rebuilds and browser refresh.
---

# Web preview

Read [`kind-local-dev`](../kind-local-dev/SKILL.md), [`docs/agent-workflows.md`](../../../docs/agent-workflows.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- Reuse the task's worktree runtime and source its generated `.devx/env`. Start the preview with
  `cargo run -p cli -- dev serve`. It builds and runs `neon`, watches Rust and catalog changes, and restarts the server
  after each successful build. Static-asset changes refresh the browser without recompiling. Keep the watcher running
  while editing, and use the worktree's brand port from `.devx/env`.
- Verify the browser loaded `/__dev/reload.js`. A source edit should appear after compilation without a manual refresh.
  This is rebuild-and-reload, not hot module replacement, so client state resets. If a failed build leaves the previous
  page visible, fix the reported error and save again.
- Authenticate through the local OIDC flow when the page requires it. Do not hand-write cookies or touch production.
- Keep browser captures under `/tmp`. Use a real browser check for behavior string tests cannot prove.
- Captures, logs, and PR material contain only synthetic or firm-owned content. Never record client data, legal files,
  real contact details, or production identifiers.

## A Project's matter pages

In a Project checkout, preview the pages `navigator.yaml` declares under `pages:` before opening a pull request:

```bash
navigator site login --host <project.host>
navigator project preview
```

- It serves `localhost` (pass `--port` to pick one) with the same `webapp::sections` components the deployment's
  `/app/projects/{code}/{slug}` route renders. Nothing is pushed to the host to preview a page.
- Edit `navigator.yaml`, `drafts/` (or `templates/`), `blobs/` pointers, or `notes/`. The page polls the working tree
  and reloads after each save. The offline `navigator project gate` checks re-run on each save, without writing, and
  their findings sit under the banner.
- Section data that lives only in Navigator, such as Deadlines, is read from the manifest's host with the stored login,
  through `GET` requests only. Without a login for that host the command refuses and prints the login command.
- A fixed banner on every page reads "This is a draft. Create a pull request to publish." It names the host and branch
  and links to the repository's new-pull-request URL. Use its lens switcher to check the lawyer, client, and clerk
  faces. The deployed route never shows the banner.
