---
name: durable-execution
description: >
  The operational contract for Neon Law Navigator's Restate-backed durable execution: keep the running workflows alive,
  diagnose why one didn't fire, and don't break them. Covers the submit-vs-run split (workflows lib / workflows-service
  bin), the service inventory, the six-hourly Heartbeat liveness canary, the three start modes, and investigating a
  stalled workflow from evidence (commit history, GitHub Actions runs, pod and Cloud logs, Google Cloud Trace, GKE, the
  Restate console), then pinning the fix with a guard test. Trigger when touching workflows-service, a *-trigger
  CronJob, or the image Containerfiles; when re-registering with Restate; when a scheduled or manual workflow "didn't
  run"; or when adding a workspace crate (it must enter the Containerfile COPY lists). This engine executes BINDING
  legal artifacts (retainer dispatch, signatures, filings). Debugging surfaces carry invocation ids and service names,
  NEVER client content. To *add* a new workflow, use create-legal-workflow.
---

# durable-execution

Durable execution runs the firm's **binding obligations**: retainer dispatch, signatures, county and NV SoS filings. If
the engine silently stops, an engagement letter doesn't go out and a deadline can slip, so an outage is a
**competence-and-diligence** concern, not a backlog item. The **Heartbeat** is the auditable control: a six-hour gap
with no Slack notice is the alarm. It runs on DBOS inside `web`
([`docs/dbos-durability.md`](../../../docs/dbos-durability.md)).

[`docs/durable-workflows.md`](../../../docs/durable-workflows.md) is authoritative for the submit-vs-run split, the
service inventory, the three start modes and idempotency keys, the registration gotcha, the two-tokens/two-ports auth
model, and the evidence-and-guardrails playbook.

## Rules

- **Diagnose by evidence, not memory.** When a workflow didn't fire, read commit history, the GitHub Actions run, the
  logs, and Google Cloud. `navigator ops doctor` triages the cluster slice fast; confirm against the evidence.
- **Fix the root cause, then guard it.** Pin every fix with a test or manifest field in the same PR.
- **Adding a workspace crate?** Add it to every workspace-building Containerfile's COPY list, or the next trigger build
  takes every trigger image down (the `cli::devx` test guards this). Image builds belong to `deploy.yml`'s daily tag
  flow.
- **Debugging logs identifiers and status only, never client content.**

## Boundaries

- *Add* a new workflow (template + questionnaire + handlers): [[create-legal-workflow]]. *Author* a Restate handler
  without breaking durability (the `ctx.run` rule): [[restate]]. Telemetry, the trigger metric, and BigQuery:
  [[observability]].
