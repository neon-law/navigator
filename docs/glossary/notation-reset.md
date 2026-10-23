---
title: "Notation Reset"
description: >-
  A Notation Reset is the daily job that deletes every Notation draft on a deployment and posts the count to Slack.
---

The daily reset of every Notation draft in a [Deployment Environment](deployment-environment.md). A draft is a
[Notation](../notation.md#notation) whose state still ranks below [Lawyer Review](lawyer-review.md) — `BEGIN` or
`intake_persisted` — with no imported document, no rendered PDF, and no signature; every stored preview draft (the unrun
`notation_draft` row) is a draft by construction. The one predicate lives in
[`store::notation_reset`](../../store/src/notation_reset.rs) and the job never restates it. A Notation at or past Lawyer
Review is never touched.

Kubernetes owns the clock and [Restate](restate.md) owns the journal: the `notation-reset-trigger` CronJob starts the
`NotationReset` [Workflow](workflow.md) at 04:20 UTC daily, keyed on the UTC date, and the Cron schedules console starts
it on demand. Each draft is deleted with its answers, events, clauses, and sources in one transaction, and the delete
re-checks the predicate, so a rerun or a crash partway through is safe. Every run posts one message to the environment's
internal messages channel — environment, counts, duration, and any failing ids — including a run that deleted nothing,
so a missing post means the job did not run.

- Predicate and delete: [`store/src/notation_reset.rs`](../../store/src/notation_reset.rs)
- Workflow: [`workflows-service/src/notation_reset.rs`](../../workflows-service/src/notation_reset.rs)
- Clock: [`docs/cronjobs.md`](../cronjobs.md)
