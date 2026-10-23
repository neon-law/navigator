# Durable workflows

How Navigator runs durable work — retainer intake, Drive sync, and scheduled operations work — on Restate and DBOS, and
how an operator tells *why one didn't run*.

> **The one rule that costs two hours when forgotten:** a registered Restate deployment is a **snapshot, not a
  subscription**. Rolling a new worker image does **not** re-register it. A service you just added (or its new handlers)
  stays invisible at the ingress — `404 "service not found"` — until you **re-register the deployment**. See [The
  registration gotcha](#the-registration-gotcha).

> **The mental model:** **Kubernetes owns the clock; Restate owns the journal.** A trigger fires an invocation once;
  Restate makes its execution durable — journals every step, retries failures, runs it to completion on the worker.

## Two sides: submit vs. run

Durable execution is split across two crates so the rest of the workspace never binds to `restate-sdk`.

| | `workflows` (lib) | `workflows-service` (bin) |
| --- | --- | --- |
| Role | **Outbound** — *submit* a job | **Inbound** — *run* the handlers |
| Who calls it | `web` and the `archives` trigger | Restate dials *into* it |
| Runtime | `InMemoryRuntime` (dev/CI) or `RestateRuntime` | the worker itself |
| `restate-sdk` | no | **yes — the only crate with it** |
| Tested via | `wiremock` (exact HTTP shape) | `cargo test -p workflows-service` |

One worker pod hosts every Restate service. New workflows bind onto the worker endpoint, never a new pod. Today it
serves two virtual objects — `notation` (questionnaire + workflow timelines on one journal) and `project-slack`
(per-Project private-channel notices: the client-view notice, and the push-to-`main` notice a Project repository's CI
reports through `POST /app/api/projects/{id}/events/main-push`, posted once per `(sha, run attempt)`) — and the durable
workflows `Archives`, `BillingCanary`, `BillingDigest`, `ReconcileInvoices`, `DriDigest`, `FinanceAggregate` ([finance
aggregate](finance-aggregate.md)), `GeneralNag`, `NotationReset`, and `NightlyProjectUpdate` with its tagged children.
The exact set is the single source of truth in `workflows_service::registry`, whose tests assert every workflow name is
PascalCase (template filenames follow the separate snake_case convention `N103` enforces) and that the registry never
drifts from the worker's actual `.bind(...)` calls — one worker, every service. In the reference deploy the worker runs
behind `workflows.your-domain.example`.

`Heartbeat` runs on DBOS. `GeneralNag` and `DriDigest` also have default-off DBOS schedules for staged migration; their
Restate services and CronJobs remain available until deployment configuration switches engines together. See
[`dbos-durability.md`](dbos-durability.md) for flags, schedule ids, recovery behavior, and the Slack delivery boundary.

The runtime is chosen by `RESTATE_BROKER_URL`: unset means in-process / in-memory, so KIND works with zero config; set
means the `RestateRuntime` adapter posts to the broker over HTTP. The same selection is used in `portal::main` and the
`archives` trigger.

## How workflows start

Restate workflows start by event, CronJob, or manual request. DBOS schedules start their workflow slots in the host
process after the executor launches.

| Mode | Fired by | Example | Code |
| --- | --- | --- | --- |
| **Event-driven** | `web`, on a user action | retainer intake; Drive sync | `portal::retainer_walk` |
| **Scheduled** | a Kubernetes `CronJob` | Archives; canary | `archives`/`billing-workflows` |
| **Manual** | a Cron schedules button | `POST /app/admin/schedules/:job/run` | `portal::cron_schedules` |

The submit shape is identical in all three: `POST {ingress}/{Service}/{key}/run` (append `/send` for one-way), with the
optional bearer. The shared helper is `workflows::start_workflow`.

The [Nightly Project Update](glossary/nightly-project-update.md) is the parent DAG for the nightly Project walk.
`NightlyProjectUpdate` is a workflow keyed on the **UTC date**, so a second fire on the same day attaches to the first
and does nothing new. A Kubernetes CronJob, `nightly-project-update-trigger`, starts it at **05:55 UTC**. Its tags are
`membership`, `slack`, `repository`, and `calendar`; the `membership`, `slack`, and `repository` children are
implemented as the workflows `NightlyProjectUpdateMembership`, `NightlyProjectUpdateSlack`, and
`NightlyProjectUpdateRepository`.

The parent journals the clock (`read-clock`), the start notice (`post-start`), the Project roster (`discover-projects`:
Project ids and an open flag only), and the outcome notice (`post-outcome`). It then calls one child per (Project, tag),
keyed `{date}:{project id}`, so a replay re-attaches to the same child. Each child runs its side effects in one
journaled step with a bounded retry; when the retries are exhausted the parent counts the failure against that tag and
continues with the next tag and Project. The children live in `workflows-service/src/nightly_project_update/`. For every
**open** Project the `membership` child:

1. creates the private Slack channel when none is recorded, adopts a private channel of the expected name when one
   exists, and replaces a recorded channel Slack no longer knows. Channels are named `project-<code>` in production and
   `staging-<code>` on a simulated-matters deployment, so the two never collide;
2. invites the Lawyer DRI and participating Clerks, preferring their recorded Slack identities and falling back to
   directory email lookup. It also includes Lawyers and Clerks in the owning Firm who have recorded Slack identities,
   skipping existing members. An identity supplies an address; role and Firm scope determine eligibility.

The `slack` child, for every **open** Project with an internal channel, runs two journaled steps on that channel only.
`upsert-canvas` writes the channel's one canvas: the synopsis (`Project.description`, else name, code, and status), the
Client DRI directory emails, the Lawyer DRI names and emails, the Portal link, and the Project's handles (code, status,
repository URL when recorded). The canvas is edited in place, so a later run never stacks a second one; `post-notice`
posts the run date, the recorded repository's latest default-branch commit date and commits link, and up to 20 open
stored Deadlines with their `due_at`, calendar role, and authority. It reports unavailable GitHub activity explicitly
and still runs after a canvas failure. This reports the latest commit, rather than a complete activity feed of pull
requests or every commit since the previous run. Stored dates are quoted; no legal dates are calculated. The worker
links the portal under `https://$NAVIGATOR_PUBLIC_HOST` when that variable is set and by path otherwise. The customer
Slack channel (`external_slack_channel_url`) is not read or written. The journal records only whether the canvas was
created, edited, or skipped; the directory emails exist only in the canvas body.

The `repository` child creates or adopts the open Project's private repository and records its URL, then reads the
committed `navigator.yaml` `version:` through the forge and grades it against the worker's own release: `current` (the
running release or newer), `stale` (older), or `unreadable` (missing, unparsable, or not a release version). The grade
and the two versions sit on the child outcome; the environment report carries the three counts. One unreadable manifest
is counted, not fatal. The reader is `rules::project_manifest`, the same one `navigator project gate` uses. On GitHub
this is detection only. With `NAVIGATOR_FORGE_BACKEND=forgejo` the child resolves the Project's own Firm client. When
the repository has a head commit, the child reconciles its Repository Governance and, for a stale pin, proposes the
`version:` bump on `navigator/update-<release>` with squash auto-merge (`cloud::forgejo_governance`). Each step is
idempotent, so a retried journal step opens no second pull request. Nothing is pushed to `main`. Participation is never
copied onto the forge. A deployment with no forge skips it. The parent retains its journal order: `membership`, `slack`,
then `repository`. A repository created by the last tag appears in the next daily Slack update.

A closed or archived Project is not updated: `membership` archives its recorded channel and nothing else runs. The
environment's `#messages` or `#staging-messages` channel gets aggregate counts only: Projects, channels, invitations,
canvases, notices, repositories, pins by status, and failures per tag, plus the first failure's message (a missing Slack
scope shows up here once, not as 21 silent counts). Email addresses and Slack identities stay inside the child's
journaled step and are not journaled or included in the report. The Slack bot needs `users:read.email`, `channels:read`,
`groups:read`, `groups:write`, `channels:write.invites`, `groups:write.invites`, and `canvases:write` (the canvas calls
are `conversations.canvases.create` and `canvases.edit`; the channel's canvas id comes from `conversations.info`, so no
canvas read scope is used) in addition to its existing `chat:write` permission. Deployment config must set
`SLACK_MESSAGES_CHANNEL_ID` and `SLACK_STAGING_MESSAGES_CHANNEL_ID` to the report channel IDs. Reinstall the Slack app
after adding scopes so the bot token receives them.

A `missing_scope` failure names both the scopes Slack wanted (`needed`) and the scopes the token carries (`provided`).
For `conversations.list`, Slack's `needed` can list `channels:read,groups:read,mpim:read,im:read`, even though the sweep
asks only for `private_channel`. The bot does not need `mpim:read` or `im:read`. Compare `provided` with the list above;
the usual gap is `groups:read` on a token installed before the sweep existed.

## Where the schedule lives

**Restate has no cron.** The archive schedule is a **Kubernetes `CronJob`** named `archives-trigger` in the `navigator`
namespace — stored in the cluster (etcd), evaluated by the kube-controller-manager, sourced from
`examples/deploy/k8s/exports/cron-archives-trigger.yaml` sets `schedule: "55 5,17 * * *"`, `timeZone: Etc/UTC`. It runs
the thin `navigator-archives-trigger` image — one `POST` to the ingress, then it exits. Restate owns the retry schedule
from there.

```text
kube CronJob (55 5,17 * * *) -> trigger -> POST /Archives/<scheduled-time>/run/send -> Restate ingress
                                                                                          | Accepted
                                                                                          v
                                                          worker runs: snapshot -> cost -> iceberg_telemetry -> email (journaled)
```

To inspect or fire the schedule by hand:

```bash
kubectl -n navigator get cronjob archives-trigger
kubectl -n navigator create job --from=cronjob/archives-trigger archives-trigger-manual-001
```

## Idempotency is the workflow key

Restate admits **at most one invocation per workflow key**. The key choice *is* the idempotency policy:

- **Archives** keys on the **UTC scheduled time** (05:55 or 17:55), so the two daily runs are distinct. A duplicate
  trigger for the same time attaches to its journal. Extraction records its file keys, promotion selects those exact
  application files, and the `email` step posts the Slack digest through the configured bot and messages channel.
- **Manual runs** key on a unique `manual-<uuid>`, so every click actually executes and notifies — a test button that
  deduped against the nightly run would look broken.

## Redriving a completed run

Restate admits at most one invocation per workflow key, so a run that *completed* — even with a bounded provider failure
such as `input_digest_mismatch` — can't be resubmitted through its normal trigger. `EmailSummary` keys on the receipt
id, and intake only submits on the inbound webhook: SendGrid never re-POSTs a message that already got a 202.

`navigator ops email-summary redrive --receipt <uuid>` recovers that case:

1. Refuses when the receipt's Slack delivery is already `confirmed` — this can never risk a second post. When the
   confirmed post was a provider-failure notice (`Failure: ...` in the channel), `--after-failed-result <actor>` vouches
   for it: the delivery row records no provider outcome, so the operator's name is stored as `reconciled_by` with
   reconciliation `redrive_after_failed_result`, and the delivery reopens only after the purge succeeds so the new run
   posts the real summary.
2. Purges the retained, completed invocation for that receipt id's workflow key.
3. Resubmits the identical `EmailSummaryRequest` intake would have built (same project, channel, provider models, and
   digest — `workflows::EmailSummaryConfig::request_for` is the one construction path both intake and a redrive use, so
   they can't diverge on run configuration the way intake and the worker once diverged on digest framing).

It never creates a second receipt, letter, or archive — those are digest-keyed in SurrealDB already and a redrive never
touches them. Output is status words only — whether anything was purged, and that the run was resubmitted — never a
receipt id, an invocation id, a summary, a letter, or any client content. The operator already supplied the receipt as
the command argument.

It reads the same environment a deployment's own `web`/worker already source — `NAVIGATOR_SURREAL_*` for the database,
`NAVIGATOR_SUMMARY_*` / `RESTATE_BROKER_URL` for the summary-lane configuration, and `RESTATE_ADMIN_URL` /
`RESTATE_ADMIN_TOKEN` / `RESTATE_AUTH_TOKEN` for Restate itself — so there is no separate `--deployment` flag to keep in
sync with it; source the target deployment's environment first, the same way an operator would before any other `ops`
command that reaches a live cluster.

## Auth: two tokens, two ports

The single most error-prone area: there are **two different credentials** on **two different ports**, and conflating
them is what silently broke prod.

- **Submit / trigger** an invocation — ingress, port **`:8080`**. Credential: `RESTATE_AUTH_TOKEN`, the Restate Cloud
  **`key_…`** API key (72 chars). Lives in the k8s secret `navigator-web-secrets`, sourced from the deployment's
  `secrets.enc.yaml` via Secret Manager.
- **Register** a deployment — admin API, port **`:9070`**. Credential: the **SSO access token** (a long JWT written by
  `restate cloud login`), in `~/.config/restate/config.toml` under `[global.cloud] access_token`.

The secret takes the **ingress `key_`**, never the SSO JWT — they look nothing alike. If `navigator-web-secrets` holds a
long `eyJ…` JWT, it is **wrong** and the ingress answers `401 Unauthenticated`. The deployment's
`deployments/<name>/secrets.enc.yaml` is the source of truth (see [deployment secrets](deployment-secrets.md)). We once
had this token drift across three places — the provider config's `key_`, Secret Manager `stub`, and a stale SSO JWT in
the k8s secret — which is the failure this section exists to prevent.

## The registration gotcha

Restate routes the ingress to **registered** services. Registration is a **snapshot of the worker's handler list at
register time** — it does not follow new deploys.

- In **KIND**, `cargo run -p cli -- ops restate register` wires the worker URL into the in-cluster broker, so the dev
  loop just works.
- In **Restate Cloud**, registration is an **explicit admin operation**. Rolling a new worker image does **not**
  re-register, so a service or handler added since the last registration is invisible at the ingress:

```text
POST :8080/Archives/<key>/run/send
404 {"message":"service 'Archives' not found, make sure to register the service before calling it."}
```

**Fix — re-register the deployment** (re-runs discovery against the live worker and picks up every service). Either use
the Restate Cloud console (your env, Deployments, Register deployment, overwrite the existing endpoint), or the admin
REST API authenticated with the SSO token:

```bash
ADMIN="https://<env>.env.<region>.restate.cloud:9070"
TOK=$(sed -n 's/^access_token = "\(.*\)"/\1/p' ~/.config/restate/config.toml | head -1)
# dry-run first — confirm the discovered service list before committing:
curl -s -X POST "$ADMIN/deployments" -H "Authorization: Bearer $TOK" \
  -d '{"uri":"https://workflows.your-domain.example/","force":true,"dry_run":true}' | jq '.services[].name'
# then commit (drop dry_run):
curl -s -X POST "$ADMIN/deployments" -H "Authorization: Bearer $TOK" \
  -d '{"uri":"https://workflows.your-domain.example/","force":true}'
```

The `restate` CLI is configured for the env but may report "Unable to connect" to `:9070` even when the host is
reachable; the admin REST API above works directly with the same SSO token.

### How `ship` re-registers (step 7d)

After rolling the service deployments, `ship` re-registers the worker so any handler added since the last registration
is reachable. Two design points:

- **It targets the real worker URL, not the placeholder.** The `navigator` CLI resolves the register target in
  precedence order: explicit `--url` → `NAVIGATOR_WORKFLOWS_URL` → derived `https://workflows.<brand.primary_domain>/` →
  the `workflows.example.com` placeholder of last resort. The derivation step exists because the 2026-06-10 ship had a
  domain configured but no explicit `NAVIGATOR_WORKFLOWS_URL`, fell through to the placeholder, and silently no-op'd the
  register. `ops ship` now passes the selected deployment's explicit `NAVIGATOR_WORKFLOWS_URL` from its `config.toml`;
  the derivation is the belt-and-suspenders default for an operator who hasn't set it.
- **It picks its transport from the environment.** When `RESTATE_ADMIN_URL` **and** `RESTATE_ADMIN_TOKEN` are both set
  (operator-session credentials exported for a headless run — never repository coordinates), `restate_register` POSTs
  `{"uri":<worker>,"force":true}` straight to the admin REST API — headless, needs no `restate cloud env configure`
  (which requires a TTY) and works with a non-expiring admin-scoped API key. Otherwise it shells out to the pinned
  `restate` CLI, which only reaches Restate Cloud when the operator has a selected environment in
  `~/.config/restate/config.toml` and a fresh SSO token; with neither, the CLI defaults to the `local` environment
  (`localhost:9070`) and the step fails. Wiring the two env vars is what makes an unattended ship re-register reliably.
- **It warns and continues — it does not gate the ship (yet).** A `:9070` admin endpoint that is firewalled from the
  operator's network, or an expired SSO token, would otherwise block every ship. Re-register is therefore best-effort: a
  failure prints `WARN: Restate re-register failed (continuing)` and the ship completes. The cost of warn-and-continue
  is a silent `404` on a *newly added* service until someone re-registers by hand; the `Notation` (retainer) service is
  already registered and unaffected. Flip step 7d to fail-the-ship only once `:9070` reachability is proven from the
  ship host and an **admin-scoped** token (`RESTATE_ADMIN_TOKEN` + `RESTATE_ADMIN_URL`, exported by the operator for a
  headless run) is wired — until then a hard gate trades a rare silent 404 for a frequent blocked ship.

## Adding a workflow

1. Author the spec in a notation template's `workflow:` frontmatter (see [notation authoring](notation-authoring.md))
   or, for non-notation flows, bind a new Restate service in `workflows-service`.
2. Signal it from `web` (event-driven) or add a trigger (scheduled / manual).
3. Ship the worker — see [GKE production](gke-prod.md) and [cloud operations](cloud-operations.md). Always ship
   `navigator-web` and `workflows-service` at one SHA.
4. **Re-register the deployment** (above) — otherwise the new service `404`s at the ingress no matter how clean the
   deploy was. `ship` registers `workflows.<domain>`. This step is invisible in `kubectl` and easy to forget.

## The heartbeat: proving the engine itself is alive

Every other scheduled workflow proves an *integration* — `Archives` proves the database and GCS are reachable,
`BillingCanary` proves Xero still agrees with us. None answers the bluntest operator question: *is durable execution
itself alive right now?*

`Heartbeat` answers it, and it runs on DBOS inside `web`, not on Restate ([DBOS durability](dbos-durability.md)). It is
two steps (`beat` → `notify`) that depend on nothing but the ops Slack webhook. It fires every six hours (00, 06, 12, 18
UTC) under the workflow id `Heartbeat-<UTC date>T<hour>`, so every replica and restart runs a slot once. Each run posts
one line to the engineering Slack channel: a heart glyph, `Durable execution OK`, and the beat timestamp.

The signal that matters most is the missing one: **a six-hour window with no heartbeat line means the DBOS executor in
`web` may be down.** Check the `web` pod logs for `DBOS launched`, then the slot's row in `dbos.workflow_status`.

## Debugging "the workflow didn't run"

Work down the chain; the break is almost always near the top:

1. **Did the trigger fire?** `kubectl -n navigator get cronjob archives-trigger` (last schedule) plus the trigger pod
   logs. For manual: did the admin button return the confirmation page or an error?
2. **Did the ingress accept it?** A `401` is a wrong or stale `RESTATE_AUTH_TOKEN`; a `404 service not found` is the
   registration gotcha — both have dedicated sections above.
3. **Did the worker run it?** Check the invocation in the Restate Cloud console (Invocations) or via the admin API; a
   failing step retries and surfaces there.
4. **Did the side effect happen?** Email transmits through SendGrid only when `NAVIGATOR_EMAIL_BACKEND=sendgrid` and
   `SENDGRID_API_KEY` are present; otherwise the worker silently uses a capturing backend that logs "sent" without
   sending. See [cloud operations](cloud-operations.md) for manifest-drift notes.

## When a workflow didn't run, follow the evidence

Don't pattern-match against a catalog of past outages — read what the system is telling you now, and trace the request
through the stages above. Four sources, in the order that usually pays off:

- **Commit history.** `git log` / `git blame` on `workflows-service`, the `*-trigger` manifests, and
  `images/Containerfile.*`. A durable-execution break is almost always a recent diff, not spontaneous — find what
  changed since the last run that worked.
- **GitHub Actions.** The `ci` and `deploy` runs (`gh run list`, `gh run view <id> --log`). Did the worker and trigger
  images actually build and publish, and did the deploy job succeed? A red or skipped deploy explains a stale image
  faster than any cluster probe.
- **Logs.** The trigger pod and worker logs (`kubectl -n navigator dev logs ...`) for the live failure; Cloud Logging /
  BigQuery for history. The worker prints its backends and per-invocation status; the trigger logs whether the ingress
  accepted the call.
- **Google Cloud.** Cloud Trace for the invocation's spans (`web` → ingress → handler), GKE workload state, and the
  Restate Cloud console's Invocations view for which step failed and retried.

`navigator ops doctor` is a shortcut for the cluster slice — it surfaces wedged trigger Jobs and unready workloads in
plain language. Triage with it, then confirm against the evidence above.

## Guardrails: keep a fixed bug fixed

We don't re-litigate the same outage. Every failure we've actually hit is pinned by a test or a manifest field so it
can't silently recur; when you fix a new one, add its guard in the same PR.

- **Every workspace member is in every workspace-building Containerfile.** Add a crate → add `COPY <crate> <crate>` to
  `images/Containerfile.{trigger,workflows-service,web}`. Guarded by
  `every_workspace_staging_containerfile_copies_every_member` in `cli/tests/containerfile_docs_copy.rs`.
- **Registered workflow names are PascalCase** and the registry matches `main.rs`'s `.bind(...)` calls — guarded by the
  `workflows_service::registry` tests (template filenames follow the separate snake_case convention `N103` enforces).
- **Trigger CronJobs carry `activeDeadlineSeconds` + `startingDeadlineSeconds`** so a stuck Job can't wedge `Forbid`,
  and `start_workflow` has a 30s HTTP timeout so a hung ingress can't keep a trigger pod alive.
- **Host `web` and the worker share one database inside their KIND tier.** The Restate worker journals each transition
  to the store its own `NAVIGATOR_SURREAL_*` names — `navigator` in that cluster. Both `dev up` and `dev worktree-env`
  put host `web` on the worker's database ([kind-local-dev](../.agents/skills/kind-local-dev/SKILL.md)), so every
  Restate-backed flow (the retainer walk, contract review) reaches the worker. Point host `web` at any other database
  and a Notation it commits is invisible to the worker, and the flow 500s with `journal: RecordNotFound`. The
  `matter_open_restate` regression in `server/tests/` proves the flow reaches the matter when the databases agree. There
  is currently no automated guard comparing host `web`'s and the worker's databases at `worktree-env status` or `ops
  doctor` time; a mismatch surfaces only as the `journal: RecordNotFound` 500 above.
- **Debugging stays identifier-and-status only — never client content** (the standing no-content rule; see
  [observability](observability.md)).

## See also

- The *what* of each individual workflow: [notation](notation.md), [retainer intake](retainer_intake.md).
- Scheduling any periodic job (the CronJob pattern, both flavors): [Scheduled jobs](cronjobs.md). Deploy and secret
  mechanics: [GKE production](gke-prod.md), [deployment secrets](deployment-secrets.md). Crate entry points:
  [`workflows/README.md`](../workflows/README.md) and [`workflows-service/README.md`](../workflows-service/README.md).

## Summary model contract

The request sends a `responseSchema` built by `workflows::summary_response_schema`, whose required properties equal the
serialized fields of `EmailSummary`, and the prompt names the same four keys. A candidate whose `finishReason` is not
`STOP` is never parsed. Its provider status begins `output_truncated:finish_reason=` and ends with the reason, the
output token count, and the thinking token count. An object the parser rejects begins `invalid_output:` and ends with
the same reason and count. Statuses carry reasons and counts only. The output budget defaults to 4,096 tokens because
thinking tokens share it.
