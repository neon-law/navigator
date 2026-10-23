---
name: triage-issue
description: >
  Deprecated plan-only entry point for one Linear issue. Use only when the user explicitly asks to triage, validate, or
  plan one issue without implementing it. Run the grounding and adjudication phases from `implement-issue`, comment the
  test-driven plan in Linear when the issue remains valid, and stop before code. Use `implement-issue` when code is
  wanted, `triage-projects` for the portfolio, and `author-linear-issue` to create or rewrite an issue.
---

# `/triage-issue`: plan one issue without code

Before reading or writing in Linear, read
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md). Use only synthetic examples; never
put client data, legal files, real contacts, or production IDs in Linear.

For implementation work, use [`implement-issue`](../implement-issue/SKILL.md) instead.

Follow [Start current][a], [Ground the issue][b], and [Adjudicate before editing][c]. Stop after the verdict unless it
is **Still valid**. Do not run the implementation phase.

For a valid issue, post the plan on the Linear issue with `save_comment` so a future worktree starts grounded:

```markdown
## Triage plan
**Verdict:** still valid — <one-line grounding>
**Covering tests:** <the test(s) that land with the change>
**Minimum implementation:** <smallest change satisfying the evidence>
**Blast radius:** <exact files a worktree should touch>
**Collisions:** <in-flight lanes touching the same files, or "none">
```

Name real files and real tests. A plan that says "update the relevant handler" forces the next agent to re-triage.

[a]: ../implement-issue/SKILL.md#start-current

[b]: ../implement-issue/SKILL.md#ground-the-issue

[c]: ../implement-issue/SKILL.md#adjudicate-before-editing
