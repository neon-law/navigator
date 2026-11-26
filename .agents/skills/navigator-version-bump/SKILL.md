---
name: navigator-version-bump
description: Open release update PRs for a Navigator workspace or land a published release when requested.
---

# Navigator version bump

Run `/navigator-version-bump <release>` from the workspace holding the Project checkouts. Use the workspace named in the
request, or use the configured root. Navigator defaults to `~/Projects`; set `NAVIGATOR_WORKSPACE_ROOT` to use another
directory. An explicit workspace `--dir` overrides the setting. Check the installed CLI's help for each verb.

1. Read the published release notes and linked migration instructions. Verify the exact tag and
   `navigator --version` before generating files. Obtain explicit authorization before promoting a release candidate to
   production.
2. Inventory immediate, visible child repositories with a root `navigator.yaml`. Inspect each manifest's
   host, configured forges and default branch, local Git and Jujutsu changes, Project instructions and existing reviews.
   Group staging and production by the configured host. Report archived repositories, empty sources, inaccessible
   origins and missing mirrors separately.
3. In a Forgejo-first workspace, verify the recorded Project source, the configured fetch and push destinations,
   repository identity, and private visibility for each matter repository. Read the current Forgejo main and compare it
   with each configured private mirror. Record any different primary, missing target, archived repository, public
   snapshot, or divergent history as a separate outcome. A change of production authority follows the supported cutover
   procedure and requires verification of the live source link, access, protection and CI.
4. For each Project, reuse its existing release review or prepare an isolated topic branch from the
   verified source base. Update the manifest, reusable workflow references, Navigator dependencies and lockfiles that
   actually pin the release. Run `navigator project repository sync-skills` with the release CLI. Apply the release's
   required repository migrations. Preserve the Project's coordinates, authored matter content, document pointers and
   visibility.
5. Review the complete diff, check for stale pins and run applicable package checks. Run
   `navigator project gate` for the repository and `navigator project gate --check` for live document verification.
   Inspect the exit status and any local repairs. Stage the reviewed release paths explicitly and sign the commit.
   Record any failed or unavailable check. Keep unrelated repairs in their own review.
6. Push the same signed release commit to the topic branch on Forgejo and every configured writable private mirror.
   Read each remote ref back. Resume a partial push by checking the existing refs and sending the missing commit to the
   outstanding destination. Preserve divergent refs for review. For a PR request, open or reuse the draft on the
   recorded source and verify its base, head and draft state. Report any outstanding Forgejo cutover.
7. Watch every required check for the reviewed head on each forge that runs checks. Record the run URL and terminal
   outcome. If the head changes, follow its replacement runs. Diagnose failures, repair release-related changes on the
   same branch and watch the new runs. Retry transient failures. Record unavailable runners, missing credentials,
   external approvals and unrelated matter findings as blockers. A request for drafts ends with open reviews and their
   check outcomes.
8. For a landing request, follow the authoritative review through approval, required checks and merge. Watch every
   required deployment job for the exact merge commit through completion. Verify any expected guard skip against its
   workflow. Fast-forward configured mirrors from the authoritative merge using the supported reconciliation path, then
   read every main ref back and compare the full commit IDs. Refresh the operator checkout when safe. Record the merge,
   deployment, mirror revisions and any outstanding local work separately. Continue monitoring queued and running jobs.
   A failed, missing or inaccessible required job keeps delivery open.

For recurring releases, key the private ledger by the release and configured repository. Record the base, reviewed head,
PR and checks. On a rerun, verify the recorded revisions and resume unfinished work. When a newer tag arrives while an
earlier draft is open, record both versions and follow the operator's supersession decision.

When the operator requests ongoing issue triage, capture reproducible automation gaps during each run. Search the
configured issue tracker for the same behavior first. File a new gap in the designated intake queue with a synthetic
example and a verifiable outcome. Record the issue ID in the private ledger. Keep matter identifiers and raw logs in the
workspace. Check the title, body, comments, links and attachments for private information and verify the saved issue.

The Gmail MCP connector is the only way agents access the mailbox. Every composed email stays an unsent draft for the
lawyer to send personally.
