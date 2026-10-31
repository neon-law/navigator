# DBOS durability

How Navigator runs [durable execution](glossary/durable-execution.md) on DBOS, the PostgreSQL-backed replacement for
[Restate](durable-workflows.md). The DBOS executor can run inside `web` or `workflows-service`; each schedule moves
individually while `workflows-service` keeps serving the Restate endpoint for every unported service.

## The pin

| Item | Value |
| --- | --- |
| Crate | `dbos` 0.5.0 (MIT), the official native Rust SDK |
| Database driver | SQLx 0.9 with ring rustls, the same build as the workspace |
| PostgreSQL | the pinned `neon-law-postgres` image ([PostgreSQL compatibility](postgres-compatibility.md)) |
| Missing from 0.5.0 | a scheduler and a transactional step |

## Where DBOS state lives

- **Database:** the same `navigator-postgres` server and `navigator` database as application tables.
- **Schema:** `dbos`. The SDK creates and migrates its own tables there on launch (`Config.migrate`).
- **Login:** `navigator_dbos`, owning only the `dbos` schema. It has no rights on `public`. Application logins have no
  rights on `dbos`. It also holds `CREATE` on the database: the SDK runs `CREATE SCHEMA IF NOT EXISTS dbos` on every
  launch, and PostgreSQL checks that right before it checks whether the schema exists.
- **Who may write it:** only the SDK. Operators read it to debug and never edit its rows by hand.

The roles Job converges the login once its Secret exists ([PostgreSQL on Kubernetes](postgres-kubernetes.md)).

## The gap between an effect and its checkpoint

A DBOS step runs its body and then commits a checkpoint row. These are two commits. A crash between them means recovery
runs the body again. Every step is therefore **at least once**.

The rule that closes the gap:

1. Every step with an effect derives an [Effect Key](glossary/effect-key.md) from its position:
   `{workflow_id}/{step_id}`. Both are deterministic on replay. `workflows::dbos_engine::effect_key()` returns it inside
   a step.
2. **A PostgreSQL write** stores the effect key in a unique column, in the same transaction as the write. Use
   `INSERT … ON CONFLICT (effect_key) DO NOTHING` or an upsert, then read the stored row back so a re-run returns the
   first result.
3. **A provider call that accepts an idempotency key** receives the effect key.
4. **A provider call without one** stays at least once. Name that in the workflow's docs, and keep the step to the
   single call so the duplicate window is only the crash between the provider's reply and the checkpoint.

`workflows-service/tests/dbos/effects.rs` proves both halves against PostgreSQL. An unkeyed effect repeats after a crash
inside that window. A keyed write lands once.

## Mapping a Restate service to DBOS

| Restate | DBOS |
| --- | --- |
| Workflow keyed by `{key}` | Workflow started with `workflow_id = "{service}-{key}"`; a second start joins the first |
| `ctx.run("name", …)` | `dbos::step("name", …)`, or `step_with` for bounded retries |
| Kubernetes `CronJob` → trigger → ingress | An in-process slot loop that starts the workflow with a slot-keyed id |
| Workflow calls a child workflow | A child workflow started with a derived id |
| Virtual object keyed by id (`notation`) | A partitioned queue, partition key = the object id, one at a time per key |
| Signal to a virtual object | `dbos::send` to the workflow; the workflow `dbos::recv`s it |
| Durable promise / awakeable | `dbos::set_event` and `dbos::get_event` |
| `ctx.sleep` | `dbos::sleep` |
| Terminal error | An application error type returned from the workflow |
| Bounded retries | `StepOptions.max_attempts`, with backoff |
| Cancel an invocation | `DBOS::cancel(workflow_id)`; a whole-run budget is `RunOptions.timeout` |
| Deployment version | `Config.app_version` = the release tag; recovery runs only on that version |

A slot-keyed id is what makes the in-process schedule safe. Restarts and several replicas all compute the same id for
the same slot, so the slot runs once.

## Lifecycle in `web`

`NAVIGATOR_DBOS_ENABLED` hosts the executor in the `web` process. Unset or `false`, `web` never touches DBOS. Any other
value refuses the boot. `neon`'s `main` boots through `portal::hosting::run_with_dbos`, which wraps the bound server in
`portal::hosting::with_dbos`. With the flag on, the boot does this:

1. Reads the DBOS login and `NAVIGATOR_RELEASE_TAG`, the application version, and fails if any is missing. The login is
   `NAVIGATOR_DBOS_HOST`, `_PORT`, `_USER`, `_PASSWORD`, and `_DATABASE`, with `_SSLMODE` (`verify-full` unless the host
   is loopback) and an optional `_ROOT_CERT`. It is never a connection-string secret. The release image bakes
   `NAVIGATOR_RELEASE_TAG` in. DBOS connections bound PostgreSQL lock waits to five seconds; application pools keep
   their own settings.
2. Binds the listeners.
3. Registers every ported workflow. Registration must finish before launch.
4. Calls `DBOS::launch`. Launch migrates the `dbos` schema and re-enqueues workflows left pending by this version.
5. Starts each schedule loop, then serves.
6. On `SIGTERM` or Ctrl-C, drains every listener, cancels scheduling, calls `DBOS::shutdown`, then joins the schedule
   tasks. Work cut off there stays pending, and the next launch of the same version recovers it.

`workflows::dbos_engine::Executor` is steps 3 to 6, behind the crate's `dbos` feature. `neon/tests/dbos_host.rs` runs
the `neon` router through it against PostgreSQL.

A separate worker remains optional: `NAVIGATOR_WORKFLOW_ENGINE=dbos` launches the shared `Executor` alongside the
`workflows-service` Restate endpoint, keeping unported services, event handlers, and virtual objects reachable while
DBOS workflows are enabled individually.

`NAVIGATOR_DBOS_BILLING_CANARY_ENABLED=true` independently registers the weekly `BillingCanary` schedule in either host.
It uses the Sunday 14:00 UTC slot as its deterministic workflow identity, so both hosts and restarted replicas join the
same run. The Xero step resolves one fixed system contact twice; the provider's stable email find-or-create behavior
prevents a second contact if the call repeats before its checkpoint. This adapter does not accept a DBOS Effect Key. The
Slack operations notice is at least once: a crash after Slack accepts it and before the `email` checkpoint can post it
again. The switch defaults off, and enabling it requires the configured Xero provider and Slack webhook. The existing
Restate service and CronJob remain in place during this staged rollout.

### Several replicas

Every DBOS host must set `NAVIGATOR_DBOS_EXECUTOR_ID` to a unique value for each concurrently running pod. The web and
worker manifests use the pod name from Kubernetes. DBOS recovery uses both this identity and the application version:
launch re-enqueues pending runs assigned to this identity and version. A new pod therefore cannot re-enqueue a run a
live peer owns. Slot-keyed workflow ids still make duplicate schedule starts join the same run. The real-PostgreSQL
executor tests prove that a live peer leaves the held run pending and that the matching identity recovers it after
shutdown.

The pod name survives a container restart in place, so that restart recovers its own pending runs. If Kubernetes
replaces the pod, the new pod name cannot recover the old identity's runs. Before enabling DBOS workflows with external
effects, operators must preserve the old identity from the pending row and perform a controlled handoff: stop all DBOS
replicas, launch one replica with `NAVIGATOR_DBOS_EXECUTOR_ID` set to the recorded old identity and the same release,
wait for its pending work to finish, then restore pod-name identity and scale the fleet back up. The SDK exposes no
stale-executor discovery or automatic replacement-pod recovery; do not edit DBOS tables. Pending ownership can be
inspected read-only with the deployment's approved database operator access:

```sql
SELECT workflow_uuid, executor_id, application_version
FROM dbos.workflow_status
WHERE application_name = 'navigator-workflows' AND status = 'PENDING'
ORDER BY executor_id, application_version, workflow_uuid;
```

Before a controlled handoff, record the `workflow_uuid` values for the abandoned executor and release as well. After
starting one replica with that identity and release, wait until each recorded workflow is terminal before restoring
pod-name identity or scaling the fleet up.

`workflows-service/tests/dbos/executors.rs` also proves recovery remains isolated by application version. Until
replacement-pod recovery is automated, deployments must account for this operator handoff before treating DBOS as a
fully unattended host for external effects.

### Turning it on and off

Both rows ship `NAVIGATOR_DBOS_ENABLED = "true"`. With it on, `ops ship` refuses the row until
`NAVIGATOR_POSTGRES_DBOS_PASSWORD` is in its `postgres-secrets.enc.yaml` and the `navigator-postgres-dbos` Secret
exists:

1. Add `NAVIGATOR_POSTGRES_DBOS_PASSWORD` to the deployment's `postgres-secrets.enc.yaml` through a pull request, beside
   the client logins.
2. Run `navigator ops secrets apply --deployment <name> --service postgres --deployments-dir .`. That writes
   `navigator-postgres-dbos`.
3. Ship. The release's roles step converges `navigator_dbos` and the `dbos` schema before `web` rolls.

Set `"false"` and ship to turn the executor off; `web` then never touches DBOS, and no heartbeat runs.

### FinanceAggregate and ReconcileInvoices handoff

The internal worker exposes authenticated, bounded DBOS start routes for `FinanceAggregate` and `ReconcileInvoices`. The
portal's manual FinanceAggregate action keeps the lawyer's existing Project authorization check and sends the canonical
Project id and code to the worker. Manual reconciliation and the scheduled trigger use the same namespaced
`ReconcileInvoices:<run-key>` identity. During transition, the matching per-workflow DBOS flag selects the engine; a
shared ingress URL alone does not switch an unported caller. When the flag is on, a missing DBOS ingress URL or
credential fails closed instead of falling back to Restate.

Before enabling FinanceAggregate ingress, apply the `dbos_workflow_request` PostgreSQL migration. It binds each global
DBOS workflow id to one workflow name and a SHA-256 fingerprint of its typed request. Retrying the same request is safe;
reusing its id for a different Project or parameter set returns `409 Conflict`. A workflow row that predates this claim
table cannot be proven to have the matching input through DBOS's public API, so ingress refuses it. Before activation,
operators must account for all existing FinanceAggregate rows and finish or otherwise explicitly reconcile pending work.
Do not assume an older run id is safe to reuse just because its report is absent. The claim row is durable and must
remain alongside DBOS history.

`NightlyProjectUpdate` keeps its UTC-date key and starts one stable DBOS child for each `(date, Project, tag)` tuple.
Membership channel resolution and invitation, Slack canvas and notice, and the parent roster/receipts use distinct
checkpoints. The trigger binary selects this worker's authenticated handoff only when
`NAVIGATOR_DBOS_NIGHTLY_PROJECT_UPDATE_ENABLED=true`; an unset or false flag keeps using Restate, while an invalid value
or missing DBOS URL/token fails closed. Start and completion notices use correlation receipts so a restart after Slack
accepts a message does not post it twice. The three Restate child services remain registered during the transition.

## Heartbeat on DBOS

`Heartbeat` runs only on DBOS. It has two steps, `beat` then `notify`. The schedule loop starts it at 00:00, 06:00,
12:00, and 18:00 UTC with the workflow id `Heartbeat-<UTC date>T<hour>`, and starts the current slot on launch. A
restart inside a slot joins that slot's run. `notify` retries five times. Slack's webhook takes no idempotency key, so
`notify` is at least once: a crash between Slack's reply and the checkpoint posts the line twice.

A workflow recovers only on an executor running the version that started it. Drain or finish pending runs before
removing a version.

No release renders a `heartbeat-trigger` CronJob, but a deployment created before the port can still hold one, because
`kubectl apply` never prunes. Every `ops ship` roll deletes it (`--ignore-not-found`) in the same step that retires
DBOS-hosted schedules' CronJobs, and the offline plan lists that deletion.

## GeneralNag rollout

DBOS adds `GeneralNag` to the shared web executor. Opt in with both `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_GENERAL_NAG_ENABLED=true`. The specific flag defaults off, so enabling Heartbeat does not start a second
daily post while the Restate CronJob remains active.

When enabled, the schedule uses `GeneralNag-<UTC date>` and starts at 01:11 UTC; a same-date start on another replica
joins the existing run. Before enabling it, configure `SLACK_BOT_TOKEN` and `SLACK_GENERAL_CHANNEL_ID`.

Switch engines in one roll: set `NAVIGATOR_DBOS_GENERAL_NAG_ENABLED = "true"` in the deployment's `config.toml` and run
`ops ship`. Ship renders the switch and `SLACK_GENERAL_CHANNEL_ID` into `web`, drops the `general-nag` Restate CronJob
from the render, and deletes the live one, because `kubectl apply` does not prune. Setting the switch back to `"false"`
and shipping restores the CronJob, which is the rollback path. Ship refuses the switch without `NAVIGATOR_DBOS_ENABLED =
"true"`.

Each notification step uses its DBOS Effect Key as Slack's `client_msg_id` and searches channel history from the
original workflow date's UTC midnight, inclusively. Recovery reuses a retained receipt before sending. It follows cursor
pages until it finds the receipt or exhausts the visible range. Limited history, malformed responses, and missing,
contradictory, or repeated pagination stop the notification step without sending. The bot needs the channel's history
scope as well as permission to post.

This proves recovery while Slack retains and exposes the receipt. Provider retention or deletion can remove that
external evidence, so this is receipt reconciliation rather than a provider exactly-once guarantee.

## DriDigest rollout

DBOS can host `DriDigest` when both `NAVIGATOR_DBOS_ENABLED=true` and `NAVIGATOR_DBOS_DRI_DIGEST_ENABLED=true`. The
service flag defaults off. Enabling the DBOS executor alone does not start another digest while the Restate CronJob
remains active. An enabled host requires `SLACK_WEBHOOK_URL` and fails boot when it is absent or blank.

The schedule uses the existing 01:11 UTC daily slot and the workflow id `DriDigest-<UTC date>`. It reads the same DRI
summary and, outside simulated-matters deployments, posts the same open-matters and pitches follow-up. The Restate
workflow and CronJob remain the rollback path until deployment configuration switches engines together.

Switch engines the same way as `GeneralNag`, with `NAVIGATOR_DBOS_DRI_DIGEST_ENABLED = "true"`; ship drops and deletes
the `dri-digest-trigger` CronJob. Ship refuses the switch unless the row's `secrets.enc.yaml` carries
`SLACK_WEBHOOK_URL`, an optional per-row key that the Restate worker never received in GKE.

The `query`, `notify`, `counts`, and `notify_counts` steps checkpoint separately. Incoming Slack webhooks have no
idempotency key: a crash after Slack accepts either notice and before its DBOS checkpoint can post that notice again.
The daily workflow id deduplicates ordinary duplicate starts; it cannot make Slack delivery exactly once.

## Late starts

A daily port names its UTC slot and what an executor that starts after it does. `GeneralNag` and `DriDigest` catch up: a
start at any time of day runs that day's slot, and a restart joins it. `NotationReset` starts today's slot only within
15 minutes of it (`workflows::dbos_engine::daily::LateStart::Within`); a later start waits for tomorrow. That window
covers a rolling update on the slot instant. It also means switching engines after the Restate CronJob ran that day
never repeats the run, so ship those switches outside the 15 minutes after the slot. An executor down for a whole window
skips that day.

## NotationReset rollout

DBOS hosts `NotationReset` when both `NAVIGATOR_DBOS_ENABLED=true` and `NAVIGATOR_DBOS_NOTATION_RESET_ENABLED=true`. The
workflow id is `NotationReset-<UTC date>` and the slot is 04:20 UTC. Steps are `started`, `discover`, one
`delete-notation-<id>` or `delete-preview-<id>` per draft, `finished`, and `post`. A delete re-run by recovery re-reads
the row and finds it gone, so the deletes need no effect key. `chat.postMessage` takes no idempotency key, so a crash
after Slack accepts the message and before its checkpoint posts it twice. An enabled host needs `SLACK_BOT_TOKEN` and
the messages channel: `SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters deployment, `SLACK_MESSAGES_CHANNEL_ID`
otherwise.

Switch engines with `NAVIGATOR_DBOS_NOTATION_RESET_ENABLED = "true"` in the row's `config.toml` and `ops ship`. Ship
renders the switch and both messages channels into `web`, drops `notation-reset-trigger` from the render, and deletes
the live CronJob. Ship refuses the switch without both channel coordinates. Setting it back to `"false"` restores the
CronJob.

## ProjectRecovery

`ProjectRecovery` is born on DBOS; it has no Restate twin. Opt in with both `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED=true`. The enabled schedule needs PostgreSQL and the archived-projects lane at
boot, and reads the optional Git bundle lane from `NAVIGATOR_FORGEJO_ARCHIVES_BUCKET`.

The schedule starts `ProjectRecovery-<UTC date>T<HH:MM>` every quarter hour. Its `unfinished` step reads up to 25
uncommitted [Project Recovery Archive](glossary/project-recovery-archive.md) generations from PostgreSQL; each then
advances in its own `archive-<generation>` step. Every write is an idempotent copy-and-verify keyed by content and path,
and the commit is one conditional update, so a crash between a step and its checkpoint repeats only safe work. A
generation that stops records its reason and waits for the next slot. See [Project recovery
archives](project-recovery-archives.md).

## RetentionPurge

`RetentionPurge` is born on DBOS. Opt in with `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_RETENTION_PURGE_ENABLED=true`, and name each Firm's authority in `NAVIGATOR_RETENTION_PURGE_AUTHORITIES`
as comma-separated `<firm-uuid>=<person-uuid>` pairs. An enabled schedule fails the boot without a well-formed map,
PostgreSQL, or the archived-projects lane. The person must be admin tier and hold the Firm's recovery-archive capability
when each chain runs; otherwise the chain is reported and left alone.

The daily run `RetentionPurge-<UTC date>` starts at 03:17 UTC and judges every step at that slot instant, however late a
recovery replays it. Its `due` step lists chains past their window; each is purged in its own `purge-<project>-<chain>`
step, which rechecks Legal Holds and eligibility from fresh rows. A run record keyed by chain and window makes a retry
join the earlier expunge; a crash between the expunge and that record can leave a second audit row for the same chain.
The `ProjectRecovery` schedule's `soft-deleted` step archives soft-deleted chains and removes their verified active
copies.

## SourceImport

`SourceImport` is born on DBOS. Opt in with `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED=true`. Every five minutes `SourceImport-<UTC date>T<HH:MM>` reads up to ten
pending [Headless Source Archive](glossary/headless-source-archive.md) imports from PostgreSQL and applies each in its
own `import-<id>` step, with a Forgejo client confined to the Project's Firm and that Firm's own token. The stored
archive and the commit's `Source-Tree:` trailer make a re-run safe after a crash or an unknown forge outcome; journals
carry import ids and reasons, never archive contents.

## PurchaseActivation

`PurchaseActivation` is born on DBOS. Opt in with `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_PURCHASE_ACTIVATION_ENABLED=true`. Every five minutes `PurchaseActivation-<UTC date>T<HH:MM>` reads up
to 25 invoiced [Purchases](glossary/purchase.md) from PostgreSQL, least recently checked first, and asks Xero about each
in its own `purchase-<id>` step. A paid one turns active; a subscription's renewals become a Xero repeating invoice
keyed on the Purchase id, so a re-run never sets up two. The purchase page runs the same check for the viewer's own open
invoices, so the schedule is the backstop, not the only path. Journals carry Purchase ids, never client content.

## DeadlineMirror

`DeadlineMirror` is born on DBOS. Opt in with `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_DEADLINE_MIRROR_ENABLED=true`. The daily run `DeadlineMirror-<UTC date>` starts at 04:03 UTC and judges
every step at that slot instant, however late a recovery replays it. Its `open-projects` step lists the deployment's
open matters from PostgreSQL; each is reconciled in its own `reconcile-<id>` step through
[store::deadline_calendar](glossary/deadline-mirror.md), which upserts and deletes the matter's mirrored calendar events
from its [Deadline](glossary/deadline.md) rows. A step whose provider refuses is recorded as a failed line and the run
continues, so one unreachable calendar cannot stall the night; recovery re-runs only the failed matters. The mirrored
events' private extended properties distinguish drift from a stale cache, so a re-run writes only what changed. Live
writes against Google Calendar require domain-wide delegation for the calendar scope — an account fact no repository
carries — and the door and the CLI verb accept `dry-run` to rehearse without them.

## Port order

Lowest risk first. Each port keeps its Restate twin until a deployment switches engines.

| # | Service | Why here |
| --- | --- | --- |
| 1 | `Heartbeat` | No database, one Slack call. Ported; its Restate service and trigger are gone. |
| 2 | `GeneralNag` | One read and Slack posts; DBOS path is opt-in and Restate remains active pending cutover. |
| 3 | `DriDigest` | Read-only digest and Slack posts; DBOS path is opt-in and Restate remains active pending cutover. |
| 4 | `BillingCanary` | Find-or-create on a fixed key, then one notice |
| 5 | `BillingDigest` | Cost read and one notice |
| 6 | `NotationReset` | Daily deletes; DBOS path is opt-in and Restate remains active pending cutover. |
| 7 | `FinanceAggregate` | Writes object storage |
| 8 | `Archives` | Nightly export; DBOS schedule and deduped notice are implemented. Keep trigger until ship handover. |
| 9 | `ReconcileInvoices` | Provider and database writes |
| 10 | `NightlyProjectUpdate` and its three children | Parent and child workflows |
| 11 | `ProjectPublisher` | Repository writes |
| 12 | `InboundMessageAlert` | Event-driven; started from `web` |
| 13 | `EmailSummary` | Email jobs |
| 14 | `SocialPublisher`, `SocialPublishReconciler` | Public posts; provider idempotency first |
| 15 | `project-slack` | Virtual object → partitioned queue |
| 16 | `notation` | Binding legal workflows; the `StateMachineRuntime` adapter, last |

`BillingDigest` runs on DBOS only when `NAVIGATOR_DBOS_BILLING_DIGEST_ENABLED=true`. Its stable workflow id is
`BillingDigest-<UTC date>`. The scheduler starts today's run from 13:00 through 13:14 UTC; otherwise it waits until
tomorrow. `ops ship` renders the switch and removes `billing-digest-trigger` from the exports overlay in the same
rollout, then deletes that CronJob after applying the new release. An enabled, configured digest requires
`SLACK_WEBHOOK_URL`. Webhook delivery has no idempotency key, so a crash after Slack accepts a message but before DBOS
checkpoints the step can repeat the notice.

## Not yet decided

- Draining in-flight Restate runs before a deployment switches engines.
