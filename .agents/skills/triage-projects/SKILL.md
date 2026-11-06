---
name: triage-projects
description: Audit Linear for portfolio contradictions, reconcile direction with shipped code, and report ready work.
---

# Triage projects

Follow [workflows](../../../docs/agent-workflows.md) and [safety](../../../docs/public-contributor-safety.md).

1. Refresh `origin/main`. Page through all Linear projects, initiatives, and issues on both teams:
   Engineering (`ENG-`) and `LAW`, whose issues lawyers file when a matter's Project repository blocks them. Correlate
   merged PRs across scoped repositories, preferring GitHub linkage. In the same pass, record every open pull request:
   repository, number, changed paths, and linked issue identifiers. Read every open issue's full description, relations,
   and conversation, plus every open project's description, milestones, and conversation. Follow pagination to
   exhaustion, including conversations; count completed, canceled, and duplicate records separately from open work.
   Include issues without a project and open issues under terminal projects. Keep a coverage ledger so a large response
   or truncated tool output cannot silently drop part of the audit.
2. Run `rtk cargo build -p cli`, relevant CLI commands against safe fixtures, and focused tests. Use raw Cargo output
   where exact diagnostics matter. Distinguish observations from inference; record commands, results, and explained
   skips.
3. Reconcile direction across the portfolio before ranking work. Compare acceptance criteria, project instructions,
   later decisions in conversations, the glossary, repository policy, shipped behavior, and active PRs. Look for
   incompatible sources of truth, ownership or access boundaries, approval requirements, lifecycle rules, migration
   sequences, release holds, and public claims. Check overlapping implementations, dependency cycles, and prerequisites
   whose cancellation leaves required scope unresolved. A completed prerequisite link can remain useful history.
   Distinguish an active contradiction from a superseded decision, an intentional exception, and a delivery or rollout
   gap. Record the conflicting requirements and evidence, the governing decision where established, and the remaining
   decision or acceptance needed. An observed rollout or merge does not establish an override of an explicit hold.
   Recommend one shared direction and execution order; do not silently settle material product, legal, access, or
   commercial choices. Reuse existing issues for unresolved conflicts and identify a decision owner or missing owner.
4. Automatically clean up Linear where the evidence is conclusive: complete issues whose full acceptance criteria
   shipped, mark clear duplicates against the surviving issue, and reconcile project status with its remaining work.
   Read each affected issue and its conversation before changing it. Record the supporting merged PR or main commit in
   Linear and report the changes made. Keep issues open when rollout, recovery proof, acceptance, or an owner decision
   remains; a merged implementation alone does not establish completion. Do not delete records or cancel unresolved
   scope. Create a new issue only for a verified uncovered gap, using
   [author-linear-issue](../author-linear-issue/SKILL.md); label unverified ideas as proposals in the report. Add
   evidence-backed reconciliation notes to affected issues. Correct stale current-direction text in projects when the
   governing decision is clear, preserving history and unresolved acceptance. Keep recommendations labeled as proposals
   rather than turning them into approved instructions.
5. Check open PRs and active worktrees. Exclude issues an open pull request already links from new implementation
   prompts, while retaining them in the portfolio audit. Do not touch a path an open pull request or active worktree
   already changes. Group the rest into 1–4 lanes without overlapping files or cross-lane dependencies. Rank open `LAW-`
   issues first: each is a lawyer blocked on live work, so it outranks an `ENG-` issue of equal priority, and ready
   `LAW-` work takes the first lane. An `ENG-` issue that blocks a `LAW-` issue inherits its rank.
6. Ask only material scope, acceptance, or access questions before finalizing prompts. Include answers and stated
   assumptions; leave required decisions unresolved until answered.

Lead the report with the recommended shared direction and the consequential contradictions. Separate settled decisions,
proposals, ready implementation, and operational acceptance. Report inventory counts, orphan issues, priority
distribution, and verification limits. Save the detailed audit and a complete remaining-issue census in Linear, with
links between them, and give the user a concise findings-and-cleanup summary with the report links. Distinguish full
inventory coverage from targeted source checks, tests, and live verification; retrieving a record or reading a PR's test
receipt does not mean its behavior was independently reproduced.

Present the portfolio as a compact table: **Project**, **Current issues**, and **New issues / next**. Group related
projects by brand or product area when useful (public brands, backend infrastructure, omnichannel Inbox, matter
workspace, drafting); grouping labels organize the report and are not tracker projects. Keep actual Linear project names
and links visible. Include active, planned, and actionable backlog projects; summarize completed and canceled projects
separately. Current issues names the remaining issue identifiers and blockers after cleanup. New issues / next
distinguishes newly created issues, proposed gaps, and the next action on existing issues. Do not invent issues to fill
a cell. Cite the evidence, summarize cleanup performed, and state coverage or verification limits. Keep private roadmap
details on Linear and in this private report, never in repository prose.

End with 1–4 copyable prompts for ready work, each containing:

- Issue identifiers, ordered scope, files, acceptance criteria, and tests.
- The open pull requests reviewed, and a statement that this prompt's files do not overlap them.
  If every ready issue collides, report that and do not emit a duplicate prompt.
- A new worktree using [implement-issue](../implement-issue/SKILL.md).
- One commit per issue, including its tests.
- One PR opened with [create-pr](../create-pr/SKILL.md). Fix failures until required local tests and PR-head CI pass,
  then end the session.
- The PR link and test evidence, or the external blocker that prevented them.
