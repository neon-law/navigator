# DBOS durability

How Navigator runs [durable execution](glossary/durable-execution.md) on DBOS, the PostgreSQL-backed replacement for
[Restate](durable-workflows.md). The DBOS executor can run inside `web` or `workflows-service`; each schedule moves
individually while `workflows-service` keeps serving the Restate endpoint for every unported service.

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

- **Database:** the configured application database on `navigator-postgres`. Fresh native tiers use the database
  recorded in their creation intent; their DBOS connection targets that same database.
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

`NAVIGATOR_DBOS_ENABLED` hosts the executor in the `web` process. Unset or `false`, `web` never touches DBOS. Any other
value refuses the boot. `neon`'s `main` boots through `portal::hosting::run_with_dbos`, which wraps the bound server in
`portal::hosting::with_dbos`. With the flag on, the boot does this:

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

A separate worker remains optional: `NAVIGATOR_WORKFLOW_ENGINE=dbos` launches the shared `Executor` alongside the
`workflows-service` Restate endpoint, keeping unported services, event handlers, and virtual objects reachable while
DBOS workflows are enabled individually.

BillingCanary, BillingDigest, ReconcileInvoices, and SandboxCreditReset are disabled on both engines. Their retained
flags cannot enable registration, timers, or manual starts. Retained trigger binaries exit without contacting a provider
or workflow ingress. A release does not cancel or purge existing runs; operators must disposition those separately.

### One application container

Set `NAVIGATOR_WORKFLOWS_IN_WEB=true`, `NAVIGATOR_DBOS_ENABLED=true`, and `NAVIGATOR_WORKFLOW_ENGINE=dbos` to host the
complete workflow registry in the application process. Each workflow's own switch still selects its engine; cohosting
does not transfer ownership of pending work. The application registers its app-only schedules alongside the worker's
adapters and launches one DBOS executor. HTTP serving starts after workflow registration, DBOS launch, and listener
binding succeed. A host failure stops both listeners; shutdown drains the application and authenticated workflow ingress
before stopping DBOS.

The combined host uses `WORKFLOWS_SERVICE_HEALTH_LISTEN` for readiness and authenticated DBOS ingress. It binds no
Restate compatibility listener and requires no Restate identity key. For calls from the same container, set
`NAVIGATOR_DBOS_INGRESS_URL=http://127.0.0.1:9083` and configure `NAVIGATOR_DBOS_INGRESS_TOKEN` on the host and callers.
External triggers need a private Service reaching that listener and the matching credential. Keep PostgreSQL and object
storage independent of the application container.

Keep the separate worker deployment until the handover gate in [the cutover inventory](dbos-cutover-matrix.md) passes.
Before selecting the combined host on a deployment, quiesce starts, account for pending runs and executor identities,
remove the separate executor, and route internal callers and readiness probes to the application's workflow listener.
The combined process exposes no Restate compatibility endpoint. Retained Restate traffic needs the separate worker, its
proxy, and existing routing until that traffic is reconciled. The shipped separate-worker Kubernetes topology remains
the default. The complete native ship profile omits and deletes the Surreal archive CronJob and its alert; accept source
recovery and retained-work disposition before selecting that profile. Mixed deployments retain the source archive.
PostgreSQL physical backups, WAL archives, and Iceberg exports retain their independent recovery requirements.

### Manual schedule controls

The manual Archives and NotationReset controls follow their respective DBOS switches. The lightweight web host starts
Archives through its locally registered handle; the combined host uses authenticated ingress. NotationReset requires
that ingress. HTTP admission uses `POST /internal/dbos/schedules`, with an allowlisted service and `manual-<UUID>` run
key. Failures never fall back to Restate, and a retry with the same key joins the same workflow. Billing controls reject
starts on both engines without submitting a request.

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

The PostgreSQL compatibility workflow runs a replacement-pod rehearsal in a disposable namespace. It first observes the
pending workflow, its application version, its executor identity, and the completed checkpoint. It waits for the first
pod to disappear before the replacement takes that identity. The replacement retrieves the existing workflow. The
rehearsal compares the journal and the synthetic effect receipt after recovery.

### Turning it on and off

Both rows ship `NAVIGATOR_DBOS_ENABLED = "true"`. With it on, `ops ship` refuses the row until
`NAVIGATOR_POSTGRES_DBOS_PASSWORD` is in its `postgres-secrets.enc.yaml` and the `navigator-postgres-dbos` Secret
exists:

1. Add `NAVIGATOR_POSTGRES_DBOS_PASSWORD` to the deployment's `postgres-secrets.enc.yaml` through a pull request, beside
   the client logins.
2. Run `hudson ops secrets apply --deployment <name> --service postgres --deployments-dir .`. That writes
   `navigator-postgres-dbos`.
3. Ship. The release's roles step converges `navigator_dbos` and the `dbos` schema before `web` rolls.

Set `"false"` and ship to turn the executor off; `web` then never touches DBOS, and no heartbeat runs.

### FinanceAggregate handoff

The internal worker exposes an authenticated, bounded DBOS start route for `FinanceAggregate`. The Portal retains the
lawyer's Project authorization check and sends the canonical Project identity. The configured selector chooses the
durable engine; a selected DBOS call fails closed on missing ingress or credentials. Invoice reconciliation starts are
disabled, including the retained identity-claim route.

## Heartbeat on DBOS

`Heartbeat` runs only on DBOS. It has two steps, `beat` then `notify`. The schedule loop starts it at 00:00, 06:00,
12:00, and 18:00 UTC with the workflow id `Heartbeat-<UTC date>T<hour>`, and starts the current slot on launch. A
restart inside a slot joins that slot's run. `notify` retries five times. Its post to `SLACK_OPS_CHANNEL_ID` carries no
receipt correlation, so `notify` is at least once: a crash between Slack's reply and the checkpoint posts the line
twice.

A workflow recovers only on an executor running the version that started it. Drain or finish pending runs before
removing a version.

No release renders a `heartbeat-trigger` CronJob, but a deployment created before the port can still hold one, because
`kubectl apply` never prunes. Every `ops ship` roll deletes it (`--ignore-not-found`) in the same step that retires
DBOS-hosted schedules' CronJobs, and the offline plan lists that deletion.

## GeneralNag rollout

On a deployment with `NAVIGATOR_DBOS_ENABLED=true`, ship defaults `GeneralNag` to the shared web executor. Set
`NAVIGATOR_DBOS_GENERAL_NAG_ENABLED=false` to keep the Restate schedule during handover.

When enabled, the schedule uses `GeneralNag-<UTC date>` and starts at 01:11 UTC; a same-date start on another replica
joins the existing run. Before enabling it, configure `SLACK_GENERAL_CHANNEL_ID` and the deployment's Slack Firm bot
([Slack](third-party-integrations.md#deployment-slack-app)).

When GeneralNag selects DBOS, `ops ship` renders the switch and `SLACK_GENERAL_CHANNEL_ID` into `web`. It omits the
`general-nag` Restate CronJob from the render and deletes the live one, since `kubectl apply` does not prune. Set the
switch to `"false"` and ship again to restore the CronJob. Selecting DBOS requires `NAVIGATOR_DBOS_ENABLED=true`.

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
remains active. The DBOS digest posts with the deployment's Slack Firm bot to `SLACK_GENERAL_CHANNEL_ID`, beside
`GeneralNag`'s open-matters notice; an enabled host fails boot when that channel is absent or blank.

The schedule uses the existing 01:11 UTC daily slot and the workflow id `DriDigest-<UTC date>`. It lists DRIs for open
Projects and, outside simulated-matters deployments, posts the open-matters and pitches follow-up. The Restate workflow
and CronJob remain the rollback path until deployment configuration switches engines together.

Switch engines the same way as `GeneralNag`, with `NAVIGATOR_DBOS_DRI_DIGEST_ENABLED = "true"`; ship drops and deletes
the `dri-digest-trigger` CronJob and renders `SLACK_GENERAL_CHANNEL_ID` into `web`. Ship refuses the switch unless the
row's `secrets.enc.yaml` carries `NAVIGATOR_RUNTIME_KMS_KEY`, which unseals the bot token.

The `query`, `notify`, `counts`, and `notify_counts` steps checkpoint separately. Its posts carry no receipt
correlation: a crash after Slack accepts either notice and before its DBOS checkpoint can post that notice again. The
daily workflow id deduplicates ordinary duplicate starts; it cannot make Slack delivery exactly once.

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
after Slack accepts the message and before its checkpoint posts it twice. An enabled host needs the messages channel:
`SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters deployment, `SLACK_MESSAGES_CHANNEL_ID` otherwise.

Switch engines with `NAVIGATOR_DBOS_NOTATION_RESET_ENABLED = "true"` in the row's `config.toml` and `ops ship`. Ship
renders the switch and both messages channels into `web`, drops `notation-reset-trigger` from the render, and deletes
the live CronJob. Ship refuses the switch without both channel coordinates. Setting it back to `"false"` restores the
CronJob.

## ProfitDigest

`ProfitDigest` runs on DBOS when the application PostgreSQL host and the deployment's messages channel are configured.
The daily run `ProfitDigest-<UTC date>` starts at 14:00 UTC and catches up on a late start. It posts the [Profit
Digest](glossary/profit-digest.md) to the messages channel: `SLACK_STAGING_MESSAGES_CHANNEL_ID` on a simulated-matters
deployment, `SLACK_MESSAGES_CHANNEL_ID` otherwise. An enabled host needs the channel, `NAV_BASE_URL`, and the
application's PostgreSQL login, and fails the boot without them.

Its steps read yesterday's `expected_cash` event, read and render the Firm rollups in `read`, publish today's
`expected_cash` event, and `post`. The post uses its Effect Key as Slack's `client_msg_id` and reuses a retained
receipt, the `GeneralNag` rule. A day with no open contingent matter posts nothing.

## Linear backlog

The Linear backlog digest is a built-in DBOS feature. It runs when the web executor has a private Slack destination and
its credentials. Set `SLACK_LINEAR_BACKLOG_CHANNEL_ID` to the destination's channel ID and provide
`LINEAR_BACKLOG_READ_KEY` and `NAVIGATOR_RUNTIME_KMS_KEY` as deployment secrets. The Linear key needs read access to
both teams. Use a separate test channel in staging.

Grant `chat:write` and `groups:history` by following [Deployment Slack app][deployment-slack-app].

[deployment-slack-app]: third-party-integrations.md#deployment-slack-app

The DBOS web executor starts `LinearBacklog-<Eastern date>` at 9 a.m. America/New_York. The snapshot reads every page
before composing the Slack post. If the executor starts late, it catches up for the current day. A retry checks for the
Slack receipt before posting again.

## ProjectRecovery

`ProjectRecovery` is born on DBOS; it has no Restate twin. Opt in with both `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED=true`. The enabled schedule needs PostgreSQL and the archived-projects lane at
boot, and reads the optional Git bundle lane from `NAVIGATOR_FORGEJO_ARCHIVES_BUCKET`.

The schedule starts `ProjectRecovery-<UTC date>T<HH:MM>` every quarter hour. Its `unfinished` step reads up to 25
uncommitted [Project Recovery Archive](glossary/project-recovery-archive.md) generations from PostgreSQL; each then
advances in its own `archive-<generation>` step. Every write is an idempotent copy-and-verify keyed by content and path,
and the commit is one conditional update, so a crash between a step and its checkpoint repeats only safe work. A
generation that stops records its reason and waits for the next slot. A final `coverage` step judges every Project, open
and closed, against its newest committed generation and records the counts and uncovered Project ids. It requests
nothing, and a failed read does not fail the slot. See [Project recovery
archives](project-recovery-archives.md#coverage).

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

## SentEmailExport

`SentEmailExport` is born on DBOS and hosted by `web`. Opt in with `NAVIGATOR_DBOS_ENABLED=true` and
`NAVIGATOR_DBOS_SENT_EMAIL_EXPORT_ENABLED=true`. An enabled schedule fails the boot without the application's PostgreSQL
login or `NAVIGATOR_ARCHIVES_BUCKET`. The daily run `SentEmailExport-<UTC date>` starts at 02:41 UTC and judges every
step at that slot instant. Its `pending` step lists the closed UTC days not yet exported. Each day is committed to the
[Sent Email Export](glossary/sent-email-export.md) Iceberg table in its own `export-<date>` step, and a final `purge`
step deletes PostgreSQL rows more than a year old whose day the ledger confirms. Every step is idempotent; a late
executor catches up. Journals carry dates and counts, never message content. See [the request-side
store](email-events-pipeline.md#the-request-side-store).

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

## Project Calendar membership

At 02:22, 08:22, 14:22, and 20:22 UTC, the DBOS web host checks Projects for Firms with a Google Workspace owner in
[Firm Calendar Settings](glossary/firm-calendar-settings.md). For an open Project without a calendar ID, the job creates
or adopts its private calendar and records the ID. It checks manually linked calendars too. Clearing the Firm owner
stops new jobs for that Firm. Each Project shares a write lock with setup and the Deadline mirror. Each retry reads
Project participation, Person details, and the owning Firm's membership in one PostgreSQL query. The job verifies the
recorded calendar ID in each eligible participant's own Google Calendar list. A removed participant's managed grant can
persist until reconciliation succeeds. The nightly Project update also reconciles membership after mirroring Deadlines.

On startup, each replica submits the most recent due slot. DBOS deduplicates by workflow, Project, and slot. If Project
discovery or submission fails, the host retries within a minute. An accepted membership workflow makes up to three
attempts for transient failures. Its output reports Person IDs and access readback.

The same calendar scheduler submits one `AttorneyBookingAvailability` workflow per registered booking calendar each hour
and at startup. It rebuilds the attorney's private Busy holds from verified current Project assignments. The attorney
selects that calendar once in the Notion booking link. A new Project, reschedule, cancellation, or assignment change
takes effect on the next successful pass. See [Attorney Calendar](glossary/attorney-calendar.md).

An independent hourly DBOS job reads iCalendar event resources through Google's CalDAV API with the delegated owner's
credentials. Enable the CalDAV API on the Workspace integration's Google Cloud project. Each new resource revision is
saved in PostgreSQL and included in the next configured [Iceberg Archive](glossary/iceberg-archive.md) export. A failed
backup is visible separately from membership repair. For a closed or archived Project, the job records the final event
inventory and waits for a published paired Iceberg generation that includes it. It then checks the current Project,
ownership, sharing rules, and event contents before deleting the secondary calendar and clearing its link. A changed
event requires a new archive. The delegated owner must have exclusive write access during retirement.

[Google's CalDAV guide](https://developers.google.com/workspace/calendar/caldav/v2/guide).

On staging, run setup twice on a synthetic Project and assign a verified Workspace Clerk or Admin. For a full UTC day,
check the four `ProjectCalendarMembership` workflow IDs for the Project and their terminal states in
`dbos.workflow_status`. In the participant's account, read the recorded ID from the Google Calendar list and check event
access. Remove the assignment and verify that reconciliation removes only its managed grants. Confirm that an external
address cannot receive access.

## Notation worker opt-in

`NAVIGATOR_DBOS_NOTATION_ENABLED=true` selects the DBOS Notation caller and registers its worker workflow when
`NAVIGATOR_WORKFLOW_ENGINE=dbos`. The caller requires `NAVIGATOR_DBOS_INGRESS_URL` and `NAVIGATOR_DBOS_INGRESS_TOKEN`;
the worker requires the same token and its PostgreSQL configuration. The switch defaults off.

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

`NAVIGATOR_WORKFLOW_ENGINE=dbos` plus `NAVIGATOR_DBOS_SOCIAL_PUBLISHER_ENABLED=true` registers the social publisher. Set
`NAVIGATOR_DBOS_SOCIAL_PUBLISH_RECONCILER_ENABLED=true` alongside it to run the five-minute reconciler. Both social
switches default off. The Restate services remain registered; this change does not switch deployed schedules.

Publisher workflow ids pair the Post id with its scheduled time. Durable waits cover scheduled publication, provider
retry backoff, container readiness, TikTok processing, and permalink reads. Recovery consumes the scheduled timer's
checkpoint even after its deadline. Provider writes reuse the shared store claim; an uncertain prior send settles as
`unknown` rather than sending again. An uncheckpointed container creation can leave an orphaned private container.

The reconciler requires `RESTATE_BROKER_URL` because pending inbound notices still use the Restate ingress, with
`RESTATE_AUTH_TOKEN` when configured. Without that URL the run fails before starting publishers. Exhausted step retries
fail the DBOS run; the Restate handler retains its own retry behavior. Keep these switches off until the deployment's
provider credentials, egress policy, and ingress dependencies are configured.

## Port order

Lowest risk first. Each port keeps its Restate twin until a deployment switches engines.

| # | Service | Why here |
| --- | --- | --- |
| 1 | `Heartbeat` | No database, one Slack call. Ported; its Restate service and trigger are gone. |
| 2 | `GeneralNag` | One read and Slack posts. Ship defaults to DBOS on deployments with the web executor. |
| 3 | `DriDigest` | Read-only digest and Slack posts; DBOS path is opt-in and Restate remains active pending cutover. |
| 6 | `NotationReset` | Daily deletes; DBOS path is opt-in and Restate remains active pending cutover. |
| 7 | `FinanceAggregate` | Writes object storage |
| 8 | `Archives` | Twice-daily export; runs in `web` behind its switch, and `ops ship` retires the trigger. |
| 10 | `NightlyProjectUpdate` and its three children | Parent and child workflows |
| 11 | `ProjectPublisher` | Repository writes |
| 12 | `InboundMessageAlert` | Event-driven; started from `web` |
| 13 | `EmailSummary` | Email jobs |
| 14 | `SocialPublisher`, `SocialPublishReconciler` | Public posts; provider idempotency first |
| 15 | `project-slack` | Virtual object → partitioned queue |
| 16 | `notation` | Binding legal workflows; the `StateMachineRuntime` adapter, last |

`Archives` runs on DBOS in `web` only when `NAVIGATOR_DBOS_ARCHIVES_ENABLED=true`. Its workflow id is `Archives:<UTC
slot>` for the 05:55 and 17:55 slots, with a 15-minute late-start window. Because `web` holds the PostgreSQL connection,
each run also exports the paired PostgreSQL coverage generation to Iceberg. `ops ship` renders the read-only archive
login and the reviewed coverage manifest with the switch, and deletes `archives-trigger`. See [Iceberg
archive](iceberg-archive.md#where-it-runs).

## Embedded development provider captures

The embedded development harness can register the same native workflows without external Slack or Xero credentials. This
selection requires complete canonical DBOS-only caller selection, the actual `dev` profile, `NAVIGATOR_CI_HARNESS=1`,
simulated matters, and embedded hosting. Partial and mixed hosts retain strict provider selection. An explicit staging
identity or invalid staging setting refuses captures. Explicit Slack or Xero configuration conflicts with this capture
profile and fails startup rather than being silently replaced. Other profiles keep their strict provider constructors.

GeneralNag, DriDigest, NotationReset, and Archives use capturing notification transports. These captures do not send
external messages or certify provider delivery. Billing automations remain disabled in this profile. Per-Firm Slack
credentials, repository providers, archive storage, and other handler requirements keep their own guards.

## Cutover acceptance

Before changing callers or retiring a Restate endpoint, reconcile pending, paused, and dead-lettered work and preserve
retained state and outcomes. Preserve the original idempotency and effect identities when retrying or recording a work
item's disposition. Ambiguous provider effects require reconciliation rather than a fresh submission. Complete [durable
object handover](durable-object-handover.md) for Notation and Project Slack, then verify native routing and recovery.
Selected flags and local captures do not establish that these operational gates have passed.

## Native deployment browser gate

The deployment integration job provisions a native KIND tier through `hudson quickstart`, with the complete native
selector profile. It verifies native admission and persisted DBOS checkpoint replay across host/image restart. The host
drains before the exact built application image starts as its nonroot user. All three hydrated browser suites run
against that image, including authenticated repository Draft provenance and restart checks. Independent practice
previews use owned PostgreSQL fixtures. These checks do not select a production deployment's engine or dispose of
retained workflow state; compatibility suites remain explicit.
