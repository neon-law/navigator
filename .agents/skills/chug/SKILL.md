---
name: chug
description: >-
  Triage the Linear portfolio and implement 15 ready issues with one subagent per issue, up to three concurrently. Use
  for /chug or an explicit request for this batch issue workflow.
---

# Chug

Run [triage-projects](../triage-projects/SKILL.md), then proceed directly to implementation. Default to 15 completed
issues and at most three issue subagents running concurrently; honor counts the user supplies. Invocation authorizes
delegating these issue implementations. Keep the parent available to coordinate and report progress.

1. Refresh `origin/main`, inspect both LAW and Engineering portfolios, and reconcile candidates against shipped code,
   merged PRs, open PR paths, and active worktrees. Follow triage-projects' ranking and evidence requirements. Propose
   tracker corrections without applying them. Its prompt-only ending is replaced by dispatch in this workflow.
2. Select independent, ready issues. Reserve each issue's exact paths before dispatch. Defer overlapping paths and
   dependencies until preceding work lands; exclude work already linked to an open PR. Keep a private session ledger of
   identifiers, verdicts, path reservations, agent/worktree assignments, PR URLs, validation, and blockers.
3. Give each issue its own subagent and isolated managed worktree from the current `origin/main`. Use the app's
   worktree tool for deliberate parallel checkouts. Each child works only in its assigned checkout and follows
   [implement-issue](../implement-issue/SKILL.md) and [create-pr](../create-pr/SKILL.md), including TDD, specialized
   skills, signed commits, content gates, UI verification when applicable, and a ready PR with a bare issue trailer.
   Never let children change the shared parent checkout or reuse another issue's agent for a different issue.
4. Dispatch up to three issues at once. Monitor results, inspect their evidence, and follow required PR-head CI.
   Fix failures through the owning issue agent. An issue counts toward the target when its implementation is complete,
   its PR is open or merged, and required CI passes. Already-shipped, duplicate, blocked, or skipped issues do not
   count; select replacements from the remaining ready queue. Release reservations only when concurrent work is safe.
5. Recheck main, open PRs, dependencies, and reservations before filling each free slot. Continue until the target is
   met or the ready queue is exhausted or an external blocker prevents progress. Report partial completion honestly;
   never fabricate work to reach the count or bypass an unresolved scope decision. Respect the repository's automatic
   merge policy and leave deployment to its dedicated workflow.
6. Clean up each task's owned runtime resources, then archive its managed worktree when the issue is done. At batch
   completion or an explicit stop, archive all remaining worktrees created by this batch. Before archiving, preserve
   unfinished work using [working-notes](../working-notes/SKILL.md) and copy needed ignored notes or artifacts into the
   parent checkout's private, git-ignored notes. Use the app's recoverable worktree archive tool; leave unrelated and
   current worktrees alone. Report completed/target counts, issue-to-PR links, test and CI evidence, cleanup, and
   blockers. Keep roadmap details and client information out of public Git and PR surfaces.

The parent coordinates issue work; it does not create sidebar chats or scheduled automations unless requested.
