# Restate to DBOS worker cutover inventory

This inventory follows services bound by `workflows-service/src/main.rs` and `workflows-service/src/registry.rs`;
registered DBOS workflows alone do not prove that callers, CronJobs, or data paths have switched engines.

## Durable workflows

- **EmailSummary** — `portal/src/inbound_email.rs` hands archived email receipts to the worker. Restate only;
  needs authenticated handoff, a PostgreSQL receipt path, and provider-accept recovery proof.
- **Archives** — `archives/src/bin/trigger.rs`. DBOS UTC-slot schedule in PR #1326;
  pending-run accounting and PostgreSQL export parity remain.
- **BillingCanary** — `billing-workflows/src/bin/trigger.rs`. DBOS implementation exists; caller selection
  and authenticated execution/recovery proof remain.
- **BillingDigest** — `billing-workflows/src/bin/billing_digest_trigger.rs`. Shipped on DBOS in #1317;
  pending Restate runs must be accounted for.
- **ReconcileInvoices** — Billing trigger, Portal schedule, and manual API. DBOS adapter and authenticated
  ingress exist; finish PostgreSQL path and legacy-run accounting.
- **DriDigest** — `workflows-service/src/bin/dri-digest-trigger.rs`. DBOS schedule exists; caller routing
  and PostgreSQL/runtime parity remain.
- **FinanceAggregate** — Portal manual action in `portal/src/api.rs`. DBOS adapter and identity-claim
  ingress exist; verify PG source, blobs, migration, and legacy-run accounting.
- **GeneralNag** — `workflows-service/src/bin/general-nag.rs`. DBOS schedule exists; caller gate and
  pending Restate accounting remain.
- **InboundMessageAlert** — `portal/src/inbox.rs`. Restate only; needs durable event identity, authenticated
  handoff, authorization preservation, and deduped notification recovery.
- **NightlyProjectUpdate** — `workflows-service/src/bin/nightly-project-update-trigger.rs`. DBOS parent,
  registered children, and authenticated date handoff exist. PostgreSQL tests cover invite and notice replay.
- **NightlyProjectUpdateMembership** — Parent tag child for channel membership and closed-channel archive.
  Stable date/Project identity; channel resolution and invite are separate safe checkpoints. Archive, create, and adopt
  crash tests remain.
- **NightlyProjectUpdateSlack** — Parent tag child for canvas and daily notice. Stable date/Project identity;
  canvas and receipt-backed notice are separate checkpoints. Canvas crash replay remains.
- **NightlyProjectUpdateRepository** — Parent tag child for repository and release governance. Stable
  date/Project identity; split surface, governance, pin, and update effects and test Forgejo recovery.
- **NotationReset** — Portal cron schedule and worker trigger. DBOS schedule exists; caller selection,
  PostgreSQL authority, and pending Restate accounting remain.
- **SocialPublisher** — `portal/src/blog_api.rs`. Restate only; needs permission-preserving handoff, stable
  publication identity, and KMS/provider receipt recovery.
- **SocialPublishReconciler** — `workflows-service/src/bin/social-publish-reconciler-trigger.rs`. Restate
  only; needs DBOS schedule/caller and provider receipt recovery.

## Virtual objects

- **notation** — Portal questionnaire and workflow APIs. Restate only; move keyed state and ordering to
  PostgreSQL transactions and cover the object events and side effects.
- **project-slack** — Project Slack channel operations. Restate only; replace keyed serialization with
  PostgreSQL transactions/locking and prove concurrent recovery.

## Release gate

Keep Restate bound until each required caller has a working DBOS path and verified engine selection. Code ports do not
prove that either engine has no pending work. Before a release disables Restate, operators must reconcile pending and
in-flight Restate invocations, apply required PostgreSQL migrations, verify complete export/parity and source/blob
references, and test restart/replay against PostgreSQL. This document authorizes no deployment or release.

## Read-only preservation audit

Run `navigator ops restate handover-audit --environment <configured-name>` against an explicitly selected Restate CLI
environment. The command reads aggregate invocation statuses, completed failure counts, state-entry counts, and
journal-entry counts across that environment. It never reads object keys, journal bodies, results, or payloads. Unknown
statuses, malformed counts, missing responses, and query failures stop the audit. Retained invocations, including
successful completions and their idempotency evidence, object state, or journals produce a nonzero exit and require
preservation and reconciliation.

These sequential, point-in-time counts are not a consistent recovery archive or permission to stop Restate. Preserve
queues, timers, promises, locks, invocation relationships, retained outcomes, and external-effect receipts separately. A
completed invocation can leave durable object state behind. Run the audit again after the final writer cutoff and
reconcile it with the verified private Restate recovery evidence before disabling the engine.
