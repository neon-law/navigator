---
name: author-linear-issue
description: Create or correct a Linear issue or project from current repository evidence.
---

# Author Linear issues

Read [`docs/agent-workflows.md`](../../../docs/agent-workflows.md),
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md), and the governing source first.

- Search for the proposed capability before calling it missing. Read its code, configuration, public API, and tests.
- Put `path:line` evidence in the observed state; name the smallest implementation and covering test.
- Keep current truth in the description and the decision trail in dated comments.
- Use only abstract or synthetic examples. Linear never receives client data, legal files, real contact details, or
  production identifiers.
- Before filing, run `navigator project feedback-check --project <Project code>` on the draft title and body (repeat
  `--project` for each matter the draft names); file the issue only when the check is clean.

Use `triage-projects` for portfolio reconciliation and `implement-issue` for one existing issue that needs code. Use
`triage-issue` only for an explicit plan-only request.
