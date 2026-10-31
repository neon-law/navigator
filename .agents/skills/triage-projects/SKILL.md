---
name: triage-projects
description: Reconcile Linear with shipped code, clean up verified stale records, and report next work by project.
---

# Triage projects

Follow [workflows](../../../docs/agent-workflows.md) and [safety](../../../docs/public-contributor-safety.md).

1. Refresh `origin/main`. Page through Linear projects, initiatives, issues, and selected conversations on both teams:
   Engineering (`ENG-`) and `LAW`, whose issues lawyers file when a matter's Project repository blocks them. Correlate
   merged PRs across scoped repositories, preferring GitHub linkage. In the same pass, record every open pull request:
   repository, number, changed paths, and linked issue identifiers.
2. Run `rtk cargo build -p cli`, relevant CLI commands against safe fixtures, and focused tests. Use raw Cargo output
   where exact diagnostics matter. Distinguish observations from inference; record commands, results, and explained
   skips.
3. Automatically clean up Linear where the evidence is conclusive: complete issues whose full acceptance criteria
   shipped, mark clear duplicates against the surviving issue, and reconcile project status with its remaining work.
   Read each affected issue and its conversation before changing it. Record the supporting merged PR or main commit in
   Linear and report the changes made. Keep issues open when rollout, recovery proof, acceptance, or an owner decision
   remains; a merged implementation alone does not establish completion. Do not delete records or cancel unresolved
   scope. Create a new issue only for a verified uncovered gap, using
   [author-linear-issue](../author-linear-issue/SKILL.md); label unverified ideas as proposals in the report.
4. Check open PRs and active worktrees. Drop any issue an open pull request already links, and do not touch a path an
   open pull request or active worktree already changes. Group the rest into 1–4 lanes without overlapping files or
   cross-lane dependencies. Rank open `LAW-` issues first: each is a lawyer blocked on live work, so it outranks an
   `ENG-` issue of equal priority, and ready `LAW-` work takes the first lane. An `ENG-` issue that blocks a `LAW-`
   issue inherits its rank.
5. Ask only material scope, acceptance, or access questions before finalizing prompts. Include answers and defaults.

Present the portfolio as a compact table with three columns: **Project**, **Current issues**, and **New issues / next**.
Group related projects by brand or product area when useful, such as public brands, backend infrastructure, omnichannel
Inbox, matter workspace, and drafting. Keep actual Linear project names and links visible; grouping labels are report
organization, not new tracker projects. Include active and planned projects and actionable backlog projects; summarize
completed and canceled projects separately. Current issues names the remaining issue identifiers and blockers after
cleanup. New issues / next distinguishes newly created issues, proposed gaps, and the next action on existing issues. Do
not invent issues merely to fill a cell. Cite the evidence, summarize cleanup performed, and state coverage or
verification limits. Keep private roadmap details on Linear and in this private report, never in repository prose.

End with 1–4 copyable prompts for ready work, each containing:

- Issue identifiers, ordered scope, files, acceptance criteria, and tests.
- The open pull requests reviewed, and a statement that this prompt's files do not overlap them.
  If every ready issue collides, report that and do not emit a duplicate prompt.
- A new worktree using [implement-issue](../implement-issue/SKILL.md).
- One commit per issue, including its tests.
- One PR opened with [create-pr](../create-pr/SKILL.md). Fix failures until required local tests and PR-head CI pass,
  then end the session.
- The PR link and test evidence, or the external blocker that prevented them.
