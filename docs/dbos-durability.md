# DBOS durability

How Navigator runs [durable execution](glossary/durable-execution.md) on DBOS, the PostgreSQL-backed replacement for
[Restate](durable-workflows.md). The application cohosts one DBOS executor and the native workflow registry. Explicit
compatibility artifacts retain the Restate endpoint for accounted-for historical work.

## The pin

| Item | Value |
| --- | --- |
| Crate | `dbos` 0.6.0 (MIT), the official native Rust SDK |
| Database driver | SQLx 0.9 with ring rustls, the same build as the workspace |
| PostgreSQL | the pinned `navigator/postgres` image ([PostgreSQL compatibility](postgres-compatibility.md)) |
| Missing from 0.6.0 | a transactional step |

## Upgrading the deployed DBOS schema

The first deployment of DBOS 0.6 requires a coordinated cutover for each database. Quiesce workflow starts and let the
0.5 runs finish before changing the release tag, which changes the DBOS application version. Stop every 0.5 executor and
client sharing the system database before starting 0.6. The first 0.6 executor migrates the schema on launch. Start any
0.6 clients after the migration. Resume workflow starts after the new executor is healthy. Use a forward fix after
migration. Redeploying a release built with 0.5 cannot roll back the schema.

If a 0.5 run must be recovered after migration, 0.6 needs the old application version and executor identity to recover
it in place. See the [upstream upgrade instructions][dbos-upgrade] and the controlled handoff below before changing
either value.

[dbos-upgrade]: <https://github.com/dbos-inc/dbos-transact-rust/tree/v0.6.0#upgrading-from-05>

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
3. **A provider call that accepts an idempotency key** receives the effect key. 4. **A provider call without one** stays
at least once. Name that in the workflow's docs, and keep the step to the
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

`neon` boots the cohosted application through `portal::hosting::run_with_dbos`. Ordinary application startup requires
admitted PostgreSQL history and durable-object ownership before launching the native registry. Backend selectors do not
bypass those checks. Startup then:

1. Reads the DBOS login and `NAVIGATOR_RELEASE_TAG`, the application version, and fails if any is missing. The login is
   `NAVIGATOR_DBOS_HOST`, `_PORT`, `_USER`, `_PASSWORD`, and `_DATABASE`, with `_SSLMODE` (`verify-full` unless the host
   is loopback) and an optional `_ROOT_CERT`. It is never a connection-string secret. The release image bakes
   `NAVIGATOR_RELEASE_TAG` in. DBOS connections bound PostgreSQL lock waits to five seconds; application pools keep
   their own settings.
2. Binds the listeners. 3. Registers every ported workflow. Registration must finish before launch. 4. Calls
`DBOS::launch`. Launch migrates the `dbos` schema and re-enqueues workflows left pending by this version. 5. Starts each
schedule loop, then serves. 6. On `SIGTERM` or Ctrl-C, drains every listener, cancels scheduling, calls
`DBOS::shutdown`, then joins the schedule
   tasks. Work cut off there stays pending, and the next launch of the same version recovers it.

`workflows::dbos_engine::Executor` is steps 3 to 6, behind the crate's `dbos` feature. `neon/tests/dbos_host.rs` runs
the `neon` router through it against PostgreSQL.

`BillingCanary` is registered for manual starts. Automatic weekly starts remain independently controlled by
`NAVIGATOR_DBOS_BILLING_CANARY_SCHEDULE_ENABLED`, default false. `ReconcileInvoices` likewise keeps
`NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED`, default false. Both controls are strict booleans; disabling a
timer leaves its authenticated manual route registered.

The canary uses the Sunday 14:00 UTC slot as its deterministic identity. Xero contact find-or-create uses a stable
provider key; the Slack notice remains at least once across a crash between acceptance and checkpoint.

### One application container

The application registers the worker's six schedule contexts alongside its eight application-owned schedule fields and
launches one DBOS executor. HTTP serving starts after registration, DBOS launch and listener binding. A host failure
stops both listeners; shutdown drains application requests and authenticated workflow ingress before stopping DBOS. This
topology does not transfer pending legacy work by itself.

The combined host uses `WORKFLOWS_SERVICE_HEALTH_LISTEN` for readiness and authenticated DBOS ingress. It binds no
Restate compatibility listener and requires no Restate identity key. For calls from the same container, set
`NAVIGATOR_DBOS_INGRESS_URL=http://127.0.0.1:9083` and configure `NAVIGATOR_DBOS_INGRESS_TOKEN` on the host and callers.
External triggers need a private Service reaching that listener and the matching credential. Keep PostgreSQL and object
storage independent of the application container.

Before deploying the native topology, quiesce starts, account for retained runs and executor identities, and complete
the handover gates in [the cutover inventory](dbos-cutover-matrix.md). The ordinary renderer excludes the separate
worker Deployment and duplicate legacy trigger Crons, but never deletes historical runtime data or PVCs. Retained
Restate traffic requires an explicitly built compatibility worker and its existing authenticated routing until
reconciled. Never discard pending work to make a topology change pass.

### Manual schedule controls

The manual Archives, BillingCanary, BillingDigest, NotationReset and ReconcileInvoices controls always use authenticated
native ingress, independently of timer consent; failure never falls back to Restate. Archives and the first three
built-in schedules use `POST /internal/dbos/schedules`, with an allowlisted service and `manual-<UUID>` run key. A retry
reuses that key and joins the existing workflow. Separate operator clicks get distinct keys. The executor's typed
schedule handles start the same workflows as the automatic schedules; manual dispatch adds no second workflow
implementation. ReconcileInvoices retains its identity-claim route.

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

### Deployment prerequisites

The ordinary full roll requires the DBOS and archive login passwords in `postgres-secrets.enc.yaml`, their projected
Secrets, native ingress bearer and real provider configuration. The ordered roles and migration steps run before
ApplicationReadiness. That gate checks persisted application history before application rollout; backend flags cannot
activate missing history. Image-only and restart retain their narrower lane semantics, while application startup still
refuses incomplete admission.

Use `hudson ops secrets apply --deployment <name> --service postgres --deployments-dir .` for the existing projected
role Secrets. Do not put login passwords in the application `envFrom` Secret or forge history receipts.

### FinanceAggregate and ReconcileInvoices handoff

The internal worker exposes authenticated, bounded DBOS start routes for `FinanceAggregate` and `ReconcileInvoices`. The
portal's manual FinanceAggregate action keeps the lawyer's existing Project authorization check and sends the canonical
Project id and code to the worker. Manual reconciliation and the scheduled trigger use the same namespaced
`ReconcileInvoices:<run-key>` identity. Missing native ingress configuration fails closed rather than selecting a
retained engine.

Before enabling FinanceAggregate ingress, apply the `dbos_workflow_request` PostgreSQL migration. It binds each global
DBOS workflow id to one workflow name and a SHA-256 fingerprint of its typed request. Retrying the same request is safe;
reusing its id for a different Project or parameter set returns `409 Conflict`. A workflow row that predates this claim
table cannot be proven to have the matching input through DBOS's public API, so ingress refuses it. Before activation,
operators must account for all existing FinanceAggregate rows and finish or otherwise explicitly reconcile pending work.
Do not assume an older run id is safe to reuse just because its report is absent. The claim row is durable and must
remain alongside DBOS history.

`NightlyProjectUpdate` keeps its UTC-date key and starts one stable DBOS child for each `(date, Project, tag)` tuple.
Membership channel resolution and invitation, Slack canvas and notice, and the parent roster/receipts use distinct
checkpoints. Its native trigger requires authenticated DBOS ingress. Start and completion notices use correlation
receipts so recovery after Slack accepts a message does not post twice. Explicit compatibility artifacts preserve
historical child services and their journal identities for retained work.

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

The native schedule retains 01:11 UTC and `GeneralNag-<UTC date>`; another same-date start joins the existing run. The
deployment requires `SLACK_BOT_TOKEN` and `SLACK_GENERAL_CHANNEL_ID`. The normal render retires the old `general-nag`
CronJob. Old backend false selected an unsuspended legacy Cron, so it was not automation consent.

Each notification step uses its DBOS Effect Key as Slack's `client_msg_id` and searches channel history from the
original workflow date's UTC midnight, inclusively. Recovery reuses a retained receipt before sending. It follows cursor
pages until it finds the receipt or exhausts the visible range. Limited history, malformed responses, and missing,
contradictory, or repeated pagination stop the notification step without sending. The bot needs the channel's history
scope as well as permission to post.

This proves recovery while Slack retains and exposes the receipt. Provider retention or deletion can remove that
external evidence, so this is receipt reconciliation rather than a provider exactly-once guarantee.

## DriDigest rollout

The native schedule retains 01:11 UTC and `DriDigest-<UTC date>`. It reads the same DRI summary and, outside simulated
matters, posts the same open-matters and pitches follow-up. The host requires `SLACK_WEBHOOK_URL`; the normal render
retires `dri-digest-trigger` after the full release admission gates.

The `query`, `notify`, `counts`, and `notify_counts` steps checkpoint separately. Incoming Slack webhooks have no
idempotency key: a crash after Slack accepts either notice and before its DBOS checkpoint can post that notice again.
The daily workflow id deduplicates ordinary duplicate starts; it cannot make Slack delivery exactly once.

## Late starts

A daily port names its UTC slot and what an executor that starts after it does. `GeneralNag` and `DriDigest` catch up: a
start at any time of day runs that day's slot, and a restart joins it. `NotationReset` starts today's slot only within
15 minutes of it (`workflows::dbos_engine::daily::LateStart::Within`); a later start waits for tomorrow. That window
covers a rolling update on the slot instant. During legacy handover, reconcile any same-day legacy start and perform the
change outside the 15-minute slot window. An executor down for a whole window skips that day.

## NotationReset rollout

The native `NotationReset` workflow id is `NotationReset-<UTC date>` and the slot is 04:20 UTC. Steps are `started`,
`discover`, one `delete-notation-<id>` or `delete-preview-<id>` per draft, `finished`, and `post`. A delete re-run by
recovery re-reads the row and finds it gone, so the deletes need no effect key. `chat.postMessage` takes no idempotency
key, so a crash after Slack accepts the message and before its checkpoint posts it twice. An enabled host needs
`SLACK_BOT_TOKEN` and the messages channel: `SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters deployment,
`SLACK_MESSAGES_CHANNEL_ID` otherwise.

The normal render supplies both messages channels and retires `notation-reset-trigger`. This preserves its existing
daily cadence rather than treating the former backend selector as a new consent control.

## ProfitDigest

`ProfitDigest` runs on DBOS when the application PostgreSQL host and the deployment's messages channel are configured.
The daily run `ProfitDigest-<UTC date>` starts at 14:00 UTC and catches up on a late start. It posts the [Profit
Digest](glossary/profit-digest.md) to the messages channel: `SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters
deployment, `SLACK_MESSAGES_CHANNEL_ID` otherwise. An enabled host needs `SLACK_BOT_TOKEN`, the channel, `NAV_BASE_URL`,
and the application's PostgreSQL login, and fails the boot without them.

Its steps read yesterday's `expected_cash` event, read and render the Firm rollups in `read`, publish today's
`expected_cash` event, and `post`. The post uses its Effect Key as Slack's `client_msg_id` and reuses a retained
receipt, the `GeneralNag` rule. A day with no open contingent matter posts nothing.

## Linear backlog

The Linear backlog digest is a built-in DBOS feature. It runs when the web executor has a private Slack destination and
its credentials. Set `SLACK_LINEAR_BACKLOG_CHANNEL_ID` to the destination's channel ID and provide
`LINEAR_BACKLOG_READ_KEY` and `SLACK_BOT_TOKEN` as deployment secrets. The Linear key needs read access to both teams.
Use a separate test channel in staging.

Grant `chat:write` and `groups:history` by following [Deployment Slack app][deployment-slack-app].

[deployment-slack-app]: third-party-integrations.md#deployment-slack-app

The DBOS web executor starts `LinearBacklog-<Eastern date>` at 9 a.m. America/New_York. The snapshot reads every page
before composing the Slack post. If the executor starts late, it catches up for the current day. A retry checks for the
Slack receipt before posting again.

## ProjectRecovery

`ProjectRecovery` is born on DBOS; it has no Restate twin. Opt in with `NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED=true`.
The enabled schedule needs PostgreSQL and the archived-projects lane at boot, and reads the optional Git bundle lane
from `NAVIGATOR_FORGEJO_ARCHIVES_BUCKET`.

The schedule starts `ProjectRecovery-<UTC date>T<HH:MM>` every quarter hour. Its `unfinished` step reads up to 25
uncommitted [Project Recovery Archive](glossary/project-recovery-archive.md) generations from PostgreSQL; each then
advances in its own `archive-<generation>` step. Every write is an idempotent copy-and-verify keyed by content and path,
and the commit is one conditional update, so a crash between a step and its checkpoint repeats only safe work. A
generation that stops records its reason and waits for the next slot. A final `coverage` step judges every Project, open
and closed, against its newest committed generation and records the counts and uncovered Project ids. It requests
nothing, and a failed read does not fail the slot. See [Project recovery
archives](project-recovery-archives.md#coverage).

## RetentionPurge

`RetentionPurge` is born on DBOS. Opt in with `NAVIGATOR_DBOS_RETENTION_PURGE_ENABLED=true`, and name each Firm's
authority in `NAVIGATOR_RETENTION_PURGE_AUTHORITIES` as comma-separated `<firm-uuid>=<person-uuid>` pairs. An enabled
schedule fails the boot without a well-formed map, PostgreSQL, or the archived-projects lane. The person must be admin
tier and hold the Firm's recovery-archive capability when each chain runs; otherwise the chain is reported and left
alone.

The daily run `RetentionPurge-<UTC date>` starts at 03:17 UTC and judges every step at that slot instant, however late a
recovery replays it. Its `due` step lists chains past their window; each is purged in its own `purge-<project>-<chain>`
step, which rechecks Legal Holds and eligibility from fresh rows. A run record keyed by chain and window makes a retry
join the earlier expunge; a crash between the expunge and that record can leave a second audit row for the same chain.
The `ProjectRecovery` schedule's `soft-deleted` step archives soft-deleted chains and removes their verified active
copies.

## SentEmailExport

`SentEmailExport` is born on DBOS and hosted by `web`. Opt in with `NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED=true`. An
enabled schedule fails the boot without the application's PostgreSQL login or `NAVIGATOR_ARCHIVES_BUCKET`. The daily run
`SentEmailExport-<UTC date>` starts at 02:41 UTC and judges every step at that slot instant. Its `pending` step lists
the closed UTC days not yet exported. Each day is committed to the [Sent Email Export](glossary/sent-email-export.md)
Iceberg table in its own `export-<date>` step, and a final `purge` step deletes PostgreSQL rows more than a year old
whose day the ledger confirms. Every step is idempotent; a late executor catches up. Journals carry dates and counts,
never message content. See [the request-side store](email-events-pipeline.md#the-request-side-store).

## SourceImport

`SourceImport` is born on DBOS. Opt in with `NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED=true`. Every five minutes
`SourceImport-<UTC date>T<HH:MM>` reads up to ten pending [Headless Source Archive](glossary/headless-source-archive.md)
imports from PostgreSQL and applies each in its own `import-<id>` step, with a Forgejo client confined to the Project's
Firm and that Firm's own token. The stored archive and the commit's `Source-Tree:` trailer make a re-run safe after a
crash or an unknown forge outcome; journals carry import ids and reasons, never archive contents.

## PurchaseActivation

`PurchaseActivation` is born on DBOS. Opt in with `NAVIGATOR_DBOS_PURCHASE_ACTIVATION_ENABLED=true`. Every five minutes
`PurchaseActivation-<UTC date>T<HH:MM>` reads up to 25 invoiced [Purchases](glossary/purchase.md) from PostgreSQL, least
recently checked first, and asks Xero about each in its own `purchase-<id>` step. A paid one turns active; a
subscription's renewals become a Xero repeating invoice keyed on the Purchase id, so a re-run never sets up two. The
purchase page runs the same check for the viewer's own open invoices, so the schedule is the backstop, not the only
path. Journals carry Purchase ids, never client content.

## DeadlineMirror

`DeadlineMirror` is born on DBOS. Opt in with `NAVIGATOR_DBOS_DEADLINE_MIRROR_ENABLED=true`. The daily run
`DeadlineMirror-<UTC date>` starts at 04:03 UTC and judges every step at that slot instant, however late a recovery
replays it. Its `open-projects` step lists the deployment's open matters from PostgreSQL; each is reconciled in its own
`reconcile-<id>` step through [store::deadline_calendar](glossary/deadline-mirror.md), which upserts and deletes the
matter's mirrored calendar events from its [Deadline](glossary/deadline.md) rows. A step whose provider refuses is
recorded as a failed line and the run continues, so one unreachable calendar cannot stall the night; recovery re-runs
only the failed matters. The mirrored events' private extended properties distinguish drift from a stale cache, so a
re-run writes only what changed. Live writes against Google Calendar require domain-wide delegation for the calendar
scope — an account fact no repository carries — and the door and the CLI verb accept `dry-run` to rehearse without them.

## Project Calendar membership

Opt in with `NAVIGATOR_DBOS_PROJECT_CALENDAR_MEMBERSHIP_ENABLED=true` on the DBOS web host after applying PostgreSQL
migrations. Setup creates or adopts the private calendar; the membership schedule discovers recorded calendars,
including those linked by hand, and checks current assignments every five minutes. Each Project gets its own DBOS
workflow and shares a write lock with setup and the Deadline mirror. The workflow retries a failed pass and the next
slot repairs drift. Check the workflow output for Person IDs, access readback, calendar notification requests, and event
response counts. A provider failure remains a failed workflow. Google Calendar reports attendee responses and accepts
notification requests; it does not provide an email delivery receipt.

On staging, run setup twice on a synthetic Project, assign a verified Workspace Clerk or Admin, and check the recorded
ID, access readback, and event response status. Change the assignment and verify that the job removes only its own
direct grants and guests. Confirm that an external address cannot receive access before enabling the job for production.

## Notation worker

The native Notation caller requires `NAVIGATOR_DBOS_INGRESS_URL` and `NAVIGATOR_DBOS_INGRESS_TOKEN`; the worker requires
the same token, admitted PostgreSQL history and imported durable-object ownership.

The DBOS worker acknowledges a Lawyer Review approval after starting a durable transition for that review revision. The
worker renders and journals the approved PDF before publishing the Notation state. A repeat approval reconciles a
Notation that still shows Lawyer Review after its Workflow advanced and the PDF was persisted. Envelope dispatch records
the draft ID before entering the signing state and checks that ID with the provider on retry. The review screen reports
a queued or failed run while the Notation still shows Lawyer Review.

The worker stores both machines in `notation_machine_state` and serializes transitions with one PostgreSQL advisory lane
per Notation. A target-only `notation_transition` pending row retains ownership across failure, cancellation, and
executor loss. The first DBOS checkpoint records its logical UUID. A second pure anchor commits before admission. To
repair a failed run, fork from its last failed step or, if no step failed, its last recorded step. Both resume and
repair forks retain the operation UUID. Another operation is refused until completion stores its result and releases
pending ownership atomically. Fresh validation failures release only before checkpointed state reads or effects; later
failures require repair. Forking before the identity checkpoint is a new operation and does not bypass a pending owner.
Importing prior state never overwrites an existing machine. Drain the Restate object and import its state before
changing traffic: its own lane does not coordinate with the DBOS lane.

Completed DBOS checkpoints are reused on recovery. A crash after an email, artifact write, journal append, or funnel
record succeeds but before its checkpoint commits can repeat that effect. These effects are at least once; the state
compare-and-set does not make them exactly once.

## Social publishing in the worker

The native registry includes the social publisher and its five-minute reconciler. Explicit compatibility services remain
available only for retained work; their settings do not select normal application routes.

Publisher workflow ids pair the Post id with its scheduled time. Durable waits cover scheduled publication, provider
retry backoff, container readiness, TikTok processing, and permalink reads. Recovery consumes the scheduled timer's
checkpoint even after its deadline. Provider writes reuse the shared store claim; an uncertain prior send settles as
`unknown` rather than sending again. An uncheckpointed container creation can leave an orphaned private container.

The native reconciler requires authenticated DBOS ingress for inbound notice recovery. Its retained SDK counterpart
preserves its legacy route. Exhausted DBOS step retries fail the run; provider credentials and egress policy remain
required, and uncertain sends remain quarantined rather than repeated.

## Port order

The native registry retains these workflow boundaries. Explicit compatibility artifacts preserve the retained handlers
until their histories are reconciled; this table does not certify live cutover.

| # | Service | Why here |
| --- | --- | --- |
| 1 | `Heartbeat` | No database, one Slack call. Ported; its Restate service and trigger are gone. |
| 2 | `GeneralNag` | One read and Slack posts. Native daily schedule. |
| 3 | `DriDigest` | Read-only digest and Slack posts; Native daily schedule; historical ownership remains guarded. |
| 4 | `BillingCanary` | Find-or-create on a fixed key, then one notice |
| 5 | `BillingDigest` | Cost read and one notice |
| 6 | `NotationReset` | Daily deletes; Native daily schedule; historical ownership remains guarded. |
| 7 | `FinanceAggregate` | Writes object storage |
| 8 | `Archives` | Twice-daily export; runs in the application; the normal renderer retires the duplicate trigger. |
| 9 | `ReconcileInvoices` | Provider and database writes |
| 10 | `NightlyProjectUpdate` and its three children | Parent and child workflows |
| 11 | `ProjectPublisher` | Repository writes |
| 12 | `InboundMessageAlert` | Event-driven; started from `web` |
| 13 | `EmailSummary` | Email jobs |
| 14 | `SocialPublisher`, `SocialPublishReconciler` | Public posts; provider idempotency first |
| 15 | `project-slack` | Virtual object → partitioned queue |
| 16 | `notation` | Binding legal workflows; the `StateMachineRuntime` adapter, last |

Native `Archives` uses `Archives:<UTC slot>` for the 05:55 and 17:55 slots, with a 15-minute late-start window. Because
`web` holds the PostgreSQL connection, each run also exports the paired PostgreSQL coverage generation to Iceberg. `ops
ship` renders the read-only archive login and reviewed coverage manifest, and deletes the duplicate `archives-trigger`.
See [Iceberg archive](iceberg-archive.md#where-it-runs).

Native `BillingDigest` uses `BillingDigest-<UTC date>`. The scheduler starts today's run from 13:00 through 13:14 UTC;
otherwise it waits until tomorrow. The normal renderer removes `billing-digest-trigger` from the exports overlay in the
same rollout, then deletes that CronJob after applying the new release. An enabled, configured digest requires
`SLACK_WEBHOOK_URL`. Webhook delivery has no idempotency key, so a crash after Slack accepts a message but before DBOS
checkpoints the step can repeat the notice.

## Invoice reconciliation timer activation

Native registration and manual ingress remain available independently of
`NAVIGATOR_DBOS_RECONCILE_INVOICES_SCHEDULE_ENABLED`, which controls the daily 11:00 UTC timer. False keeps manual DBOS
reconciliation available. The normal application defaults the timer off even outside ship; ship explicitly renders false
unless deployment coordinates opt in with true. Malformed timer settings are refused before live provider construction.
The offline ship plan reports both timer states.

## Embedded development provider captures

The embedded development harness can register the same native workflows without external Slack or Xero credentials. This
selection requires the embedded host, actual `dev` profile, `NAVIGATOR_CI_HARNESS=1`, simulated matters, and embedded
hosting. Partial and mixed hosts retain strict provider selection. An explicit staging identity or invalid staging
setting refuses captures. Explicit Slack or Xero configuration conflicts with this capture profile and fails startup
rather than being silently replaced. Other profiles keep their strict provider constructors.

GeneralNag, DriDigest, NotationReset, and Archives use the existing capturing notification transports. BillingCanary
uses the existing billing stub and capturing email; ReconcileInvoices runs the shared reconciliation operation with that
stub and its admitted PostgreSQL store. These captures do not send external messages or certify provider delivery.
Registration does not activate the canary or reconciliation timers; their independent schedule controls still apply.
Per-Firm Slack credentials, repository providers, archive storage, and other handler requirements keep their own guards.

## Cutover acceptance

Before changing callers or retiring a Restate endpoint, reconcile pending, paused, and dead-lettered work and preserve
retained state and outcomes. Preserve the original idempotency and effect identities when retrying or recording a work
item's disposition. Ambiguous provider effects require reconciliation rather than a fresh submission. Complete [durable
object handover](durable-object-handover.md) for Notation and Project Slack, then verify native routing and recovery.
Selected flags and local captures do not establish that these operational gates have passed.
