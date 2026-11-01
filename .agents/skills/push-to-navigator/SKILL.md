---
name: push-to-navigator
description: >-
  Land a Project repository change and follow the exact merge commit's deployment run to a verdict. Use for "push to the
  navigator" or when a change must be verified through the live gate.
---

# Push to Navigator

## Find the term first

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

## Deliver

Review the requested diff and run `navigator project gate` before delivery. Keep document bytes, private notes,
credentials, and client data out of Git. Use [pull-changes](../pull-changes/SKILL.md) to update the topic branch.

Run `navigator project repository push --title '<commit subject>' --branch '<topic branch>'` from the Project root. The
verb stages the checkout's changes, signs the commit, opens a ready review request, follows the required check and
merge, and watches the exact merge commit's deployment run. Reuse the branch after a recoverable stall.

The origin selects the forge. For Forgejo, configure `NAVIGATOR_FORGEJO_ROOT_URL` and `FORGEJO_TOKEN` on the trusted
host. The token must stay outside Project workflows. Forgejo scheduling is acknowledged by its API; only observing the
actual merge proves completion. The generated Forgejo workflow currently gates pull requests only, and seed writes
through Forgejo CI remain unavailable. Report its missing post-merge run as unverified; keep source handover pending.

Read the report's review URL, merge commit, deployment URL, conclusion, and job results. A failed or missing deployment
is unresolved work even after merge. Portal publishing is retired; a successful `publish` no-op proves no application.
Report any unverified stage explicitly. Do not change source authority or remove the other forge as part of this verb.
