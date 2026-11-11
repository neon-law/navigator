# Restate to DBOS worker cutover inventory

This inventory follows services bound by `workflows-service/src/main.rs` and `workflows-service/src/registry.rs`;
registered DBOS workflows alone do not prove that callers, CronJobs, or data paths have switched engines.

## How to read this checklist

Implementation and caller selection are separate from deployment acceptance. A DBOS workflow can be registered while its
callers still select Restate. Optional workflow switches default off. External callers selecting DBOS require the
authenticated ingress URL and token and fail closed when either is missing. Keep the Restate bindings until the handover
gate below passes. Source and integration-test coverage do not establish which engine a deployment currently uses.

## Durable workflows

- **EmailSummary** — DBOS workflow, authenticated ingress, receipt-keyed caller handoff. Remaining: PostgreSQL
  receipt path, provider recovery, enablement and retained runs.
- **Archives** — DBOS UTC-slot workflow and worker schedule. Remaining: Complete export/parity, deployed schedule
  selection and retained runs.
- **BillingCanary** — DBOS workflow and opt-in schedule. Remaining: Provider-effect recovery, deployed selection and
  retained runs.
- **BillingDigest** — DBOS workflow and opt-in schedule. Remaining: Deployed selection and retained runs.
- **ReconcileInvoices** — DBOS adapter, identity-claim ingress and caller selection. Remaining: PostgreSQL source
  parity, deployed selection and retained runs.
- **DriDigest** — DBOS schedule and trigger selection. Remaining: PostgreSQL/runtime parity, deployed selection and
  retained runs.
- **FinanceAggregate** — DBOS adapter, identity-claim ingress and Portal caller selection. Remaining: PostgreSQL
  source/blob parity, migrations and retained runs.
- **GeneralNag** — DBOS schedule and trigger selection. Remaining: Deployed selection, effect recovery and retained
  runs.
- **InboundMessageAlert** — DBOS workflow, authenticated ingress and Message-keyed caller selection. Remaining:
  Notification recovery, deployed selection and retained runs.
- **NightlyProjectUpdate** — DBOS parent, tagged children and authenticated date handoff. Remaining: Deployed
  selection and retained parent/child runs.
- **NightlyProjectUpdateMembership** — DBOS child with separate channel-resolution and invite checkpoints.
  Remaining: Archive/create/adopt recovery and retained runs.
- **NightlyProjectUpdateSlack** — DBOS child with separate canvas and receipt-backed notice checkpoints. Remaining:
  Canvas recovery and retained runs.
- **NightlyProjectUpdateRepository** — DBOS child for repository and release governance. Remaining: Split effect
  recovery, Forgejo parity and retained runs.
- **NotationReset** — DBOS schedule and trigger selection. Remaining: PostgreSQL authority, deployed selection and
  retained runs.
- **SocialPublisher** — DBOS workflow, authenticated ingress and publication-keyed caller selection. Remaining:
  KMS/provider receipts, cancellation, deployed selection and retained runs.
- **SocialPublishReconciler** — DBOS schedule, authenticated ingress and trigger selection. Remaining: Provider
  receipt recovery, deployed selection and retained runs.
- **SandboxCreditReset** — DBOS month-keyed adapter and authenticated trigger selection. Each overage has a separate
  attempt anchor and invoice receipt checkpoint. Automatic retry stops before Xero's request-key window expires.
  Remaining: PostgreSQL ledger parity, expired-attempt reconciliation, deployed selection and retained runs.
- **InboundTriage** — DBOS adapter and authenticated Message-keyed ingress shared by the Portal and YouTube poller.
  Remaining: PostgreSQL triage storage authority, deployed selection and retained runs.
- **YoutubeCommentsPoller** — DBOS adapter with one checkpoint per video and one run per 15-minute UTC slot.
  Remaining: PostgreSQL publication and credential parity, deployed selection and retained runs.

The implementations and bindings are in `workflows-service/src/main.rs`, `dbos_*.rs`, and `registry.rs`. Shared caller
selection lives in `workflows/src/trigger.rs`; callers include the Portal, trigger binaries, CLI, and webapp. Verify the
configured switch at every entry point before removing a trigger or binding.

For InboundTriage, set `NAVIGATOR_DBOS_INBOUND_TRIAGE_ENABLED=true` on the DBOS worker and the Portal. For the YouTube
poller, set `NAVIGATOR_DBOS_YOUTUBE_COMMENTS_POLLER_ENABLED=true` on the worker and the poll trigger. Configure the
worker's ingress token and give both callers the matching token and ingress URL. The poller submits triage through the
same selector as the Portal, so its worker also needs the ingress URL when triage uses DBOS.

For SandboxCreditReset, enable `NAVIGATOR_DBOS_SANDBOX_CREDIT_RESET_ENABLED=true` on the worker and monthly trigger
before a new, unprocessed billing month. Reconcile any Restate attempt before transferring ownership of a month. An
expired DBOS attempt without a receipt requires operator reconciliation with Xero before an invoice can be retried.

## Virtual objects

- **notation** — DBOS transition workflow, PostgreSQL state/locking and authenticated routes. Remaining: Complete
  data/effect parity, concurrent signals, deployed selection and retained object state.
- **project-slack** — DBOS workflow, authenticated ingress and caller selection. Remaining: Concurrent recovery,
  provider effects, deployed selection and retained object state.

`workflows-service/src/dbos_notation.rs` and `dbos_project_slack.rs` implement the object ports; `dbos_ingress.rs`
exposes their authenticated routes. Their presence does not prove old object state or invocations can be discarded.

## PostgreSQL data gate

Apply ordered SQLx migrations before boot. A mirror copies data without transferring write authority. Select each
domain's supported PostgreSQL accessor only after parent rows, complete source-field coverage, application parity, and
rollback disposition are verified. See [storage models and writes](storage-models.md).

Run the read-only [Mirror Backfill](postgres-tooling.md#mirror-backfill) preflight against the complete source. Account
for unexpected populated tables, explicit derived/metadata exclusions, exact identities, references, timestamps, and
object bytes. Rehearse bounded import, interruption/resume, idempotency and reconciliation before changing authority. An
archive of raw rows is not a canonical application import.

## Release gate

Keep Restate bound until each required caller has a working DBOS path and verified engine selection. Code ports do not
prove that either engine has no pending work. Before a release disables Restate, operators must reconcile pending and
in-flight Restate invocations, apply required PostgreSQL migrations, verify complete export/parity and source/blob
references, and test restart/replay against PostgreSQL. This document authorizes no deployment or release.

## Read-only preservation audit

Run `navigator-deploy ops restate handover-audit --environment <configured-name>` against an explicitly selected Restate
CLI environment. The command reads aggregate invocation statuses, completed failure counts, state-entry counts, and
journal- entry counts across that environment. It never reads object keys, journal bodies, results, or payloads. Unknown
statuses, malformed counts, missing responses, and query failures stop the audit. Retained invocations, including
successful completions and their idempotency evidence, object state, or journals produce a nonzero exit and require
preservation and reconciliation.

These sequential, point-in-time counts are not a consistent recovery archive or permission to stop Restate. Preserve
queues, timers, promises, locks, invocation relationships, retained outcomes, and external-effect receipts separately. A
completed invocation can leave durable object state behind. Run the audit again after the final writer cutoff and
reconcile it with the verified private Restate recovery evidence before disabling the engine.
