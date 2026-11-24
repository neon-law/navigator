---
name: navigator-version-bump
description: Open release update PRs for a Navigator workspace or land a published release when requested.
---

# Navigator version bump

Run `/navigator-version-bump <release>` from the workspace holding the Project checkouts. Use the workspace named in the
request or the current workspace root. Navigator selects `~/Projects` on a fresh installation and retains a sole
existing `~/Work`. When both exist, pass the intended root explicitly to commands with a workspace `--dir`. Check the
installed CLI's help for each verb.

1. Read the published release notes and linked migration instructions. Verify the exact tag and
   `navigator --version` before generating files. Obtain explicit authorization before promoting a release candidate to
   production.
2. Inventory immediate, visible child repositories with a root `navigator.yaml`. Inspect each manifest's
   host, configured forges and default branch, local Git and Jujutsu changes, Project instructions and existing reviews.
   Group staging and production by the configured host. Report archived repositories, empty sources, inaccessible
   origins and missing mirrors separately.
3. For each Project, reuse its existing release review or prepare an isolated topic branch from the
   current authoritative base. Update the manifest, reusable workflow references, Navigator dependencies and lockfiles
   that actually pin the release. Run `navigator project repository sync-skills` with the release CLI. Apply the
   release's required repository migrations. Preserve the Project's coordinates, authored matter content, document
   pointers and visibility.
4. Review the complete diff, check for stale pins and run applicable package checks. Run
   `navigator project gate` for the repository and `navigator project gate --check` for live document verification.
   Inspect the exit status and any local repairs. Stage the reviewed release paths explicitly and sign the commit.
   Record any failed or unavailable check. Keep unrelated repairs in their own review.
5. For a request for PRs, open one draft on each changed repository's authoritative forge.
   Verify its base, head, release and draft state. Reuse an existing review on a retry. Leave other work and reviews
   intact. Check all pins and canonical files before counting a repository as current.
6. When explicitly asked to land the release, follow each Project's push instructions, required checks
   and review policy. Verify deployment on the merge revision and reconcile configured mirrors. Report PR coverage,
   merges and deployments. Keep the per-repository ledger and follow-ups in the workspace's private operator records.

For recurring releases, key the private ledger by the release and configured repository. Record the base, reviewed head,
PR and checks. On a rerun, verify the recorded revisions and resume unfinished work. When a newer tag arrives while an
earlier draft is open, record both versions and follow the operator's supersession decision.

When the operator requests ongoing issue triage, capture reproducible automation gaps during each run. Search the
configured issue tracker for the same behavior first. File a new gap in the designated intake queue with a synthetic
example and a verifiable outcome. Record the issue ID in the private ledger. Keep matter identifiers and raw logs in the
workspace.

The Gmail MCP connector is the only way agents access the mailbox. Every composed email stays an unsent draft for the
lawyer to send personally.
