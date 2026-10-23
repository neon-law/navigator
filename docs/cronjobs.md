# Scheduled jobs (CronJobs)

How Neon Law Navigator runs anything on a clock. Every scheduled job is a **Kubernetes `CronJob`** in the `navigator`
namespace. **Kubernetes owns the clock.**

Which jobs ship is the `resources:` list in `examples/deploy/k8s/exports/kustomization.yaml`, which is the same list
that `kubectl kustomize` resolves when `ops ship` reconciles a cluster. A manifest sitting in that directory and named
by no `resources:` entry renders nowhere. This page deliberately does not restate the list, because prose cannot be
bound to it by a test: read the kustomization itself, or the lawyer console at `/app/admin/schedules`, which derives its
table from that list. What this page explains is how a job gets onto it.

GitHub Actions is **not** a scheduler here. CI/CD on GitHub does exactly one thing for the runtime: build and push
images. Anything that runs on a schedule is a k8s `CronJob` in the cluster — never a GitHub `schedule:` trigger — so the
clock lives next to the workload it drives, with the same secrets, network, and Restate ingress.

A k8s `CronJob` is the right scheduler because it is **stateless and self-healing**: it fires every period regardless of
what happened last time, so a failed or missed run never breaks the next one. (A self-rescheduling timer inside an app
is a chain — one broken link and it silently stops forever.) See
[durable-workflows.md](durable-workflows.md#where-the-schedule-lives) for the full "why k8s, not Restate" reasoning.

## Two flavors — pick by what the job does

| | **A. Durable-workflow trigger** | **B. Self-contained batch** |
| --- | --- | --- |
| Shape | a tiny binary `POST`s the Restate ingress | one binary does the whole job in-process |
| Use when | multi-step, exactly-once, can't-lose-it | idempotent batch that re-runs cleanly |
| Example | `archives-trigger` → `Archives`; `billing-canary-trigger` → `BillingCanary` | a cache refresh or log prune |
| Failure unit | Restate replays the failed step | the whole job re-runs next period |
| Docs | [Durable workflows](durable-workflows.md) | this page |

**Default to B for a *pure* one-shot.** Reach for A when a mid-run crash would *lose or duplicate something
irreversible* (a filing, a payment, a signature) — **or** when you want the run itself to be a durable, self-reporting
multi-step. The twice-daily **Archives export** is flavor A (`snapshot → Iceberg promotion → Slack digest`): a flaky
Slack delivery retries the digest step without re-running extraction. A trivial report or cache refresh that needs no
notification stays flavor B — it has far fewer moving parts (no worker, no Restate registration, no ingress auth), and
routing it through Restate only adds the failure surfaces (registration, token) the `archives` cutover tripped on. Weigh
the durable-two-step benefit against that cost.

## Anatomy of a CronJob

Everything is Rust and env-driven — no per-deployment value is baked into a committed manifest.

1. **A Rust binary** — a new workspace crate, a `[[bin]]` on an existing crate, or a `cli` subcommand (see
   `AGENTS.md`). Flavor A is a thin "POST and exit"; flavor B does the work and exits non-zero on failure so the Job is
   marked failed.
2. **An image** — servers from `images/Containerfile.<name>`; triggers from the shared `images/Containerfile.trigger`
   (one `--build-arg CRATE=`/`BIN=` row). CI (`deploy.yml`) builds and publishes it to Artifact Registry as
   `us-west4-docker.pkg.dev/example-project/navigator/navigator-<name>`, tagged with the release version plus `latest`.
   The repository is private and deletes images 7 days after push, so the node identity needs
   `roles/artifactregistry.reader` and a tag older than a week cannot be pulled; see
   [`gke-prod.md`](gke-prod.md#deploy-flow). CI owns image builds — there is no per-image `cargo run -p cli --
   image-<name>` build command — and the local KIND loop **pulls** them (`navigator dev deploy` / `dev worktree-env
   --demo`).
3. **A manifest** under `examples/deploy/k8s/exports/` with placeholders (`YOUR_PROJECT_ID` for the environment,
   `YOUR_IMAGES_PROJECT_ID` for the registry the image comes from, the image tag, any ingress URL), namespace
   `navigator`. Render real values at apply time; keep the committed file generic.
4. **Secrets** via `secretKeyRef` from `navigator-web-secrets`, marked `optional: true` so KIND (which doesn't apply
   these prod manifests) never blocks on a missing key.

A minimal flavor-B skeleton:

```yaml
apiVersion: batch/v1
kind: CronJob
metadata:
  name: nrs-scraper
  namespace: navigator
spec:
  schedule: "0 10 * * 0"          # Sunday 02:00 PST (10:00 UTC); see the timezone note below
  concurrencyPolicy: Forbid       # never overlap a long scrape with the next fire
  successfulJobsHistoryLimit: 7
  failedJobsHistoryLimit: 7
  jobTemplate:
    spec:
      backoffLimit: 2
      template:
        spec:
          restartPolicy: OnFailure
          containers:
            - name: nrs-scraper
              image: us-west4-docker.pkg.dev/example-project/navigator/navigator-nrs-scraper:YY.M.D
              envFrom:
                - secretRef:
                    name: navigator-web-secrets   # store credentials, storage creds, etc.
```

### Timezone convention

`spec.schedule` is **always UTC** (k8s evaluates cron in UTC unless you set `spec.timeZone`). The workspace convention
is to write the comment in Pacific and the expression in UTC:

- `archives-trigger` uses `55 5,17 * * *` with `timeZone: Etc/UTC`: 05:55 and 17:55 UTC every day.
- `0 10 * * 0` = **02:00 PST Sunday** (`0` = Sunday), for the weekly example above.

PST is UTC−8; during PDT (UTC−7) the same expression lands at 03:00 local. If a job must hit exactly 02:00 *local*
year-round, set `spec.timeZone: "America/Los_Angeles"` instead of doing the math.

When the requested time is explicitly UTC, keep both the schedule comment and expression in UTC. The
`nightly-project-update-trigger` runs at `55 5 * * *` with `timeZone: Etc/UTC` every day and starts the
`NightlyProjectUpdate` workflow keyed on the UTC date. The parent walks every Project and runs the `membership`,
`slack`, and `repository` child workflows; the [Nightly Project Update](glossary/nightly-project-update.md) defines the
full tag set. This is 21:55 fixed PST (UTC−8); it does not track daylight saving time.

## Build and deploy

CI owns image publishing. Cron trigger images are built and pushed by `deploy.yml` to
`us-west4-docker.pkg.dev/example-project/navigator/navigator-<name>` tagged with the release version + `latest` — the
same CI-published flow as `navigator-web` and `workflows-service`, never a local `docker build` + push side channel.
Nothing is built on a laptop, and the login is a Workload Identity token (see
[`gitops.md`](gitops.md#keyless-pushes-to-artifact-registry)). Deploying a cron job is therefore just: pin the manifest
to the published tag and apply.

```bash
# The deployment's coordinates are plaintext TOML — export them from its config:
set -a; eval "$(grep ' = "' deployments/<name>/config.toml | sed 's/ = /=/')"; set +a
TAG=$(git ls-remote --tags --refs origin | grep -oE '[0-9]{2}\.[0-9]{2}\.[0-9]{2}$' | sort | tail -1)  # latest release

# Render placeholders to a temp file (keep the committed manifest generic), then apply:
sed -e "s|YOUR_PROJECT_ID|${NAVIGATOR_GCP_PROJECT_ID}|g" -e "s|:YY.M.D|:${TAG}|g" \
  examples/deploy/k8s/exports/cron-<name>.yaml > /tmp/cron-<name>.yaml
kubectl apply -f /tmp/cron-<name>.yaml
```

> The daily tag flow publishes `navigator-web` and `navigator-workflows-service` today; a new cron *trigger* image must
  be added to `deploy.yml`'s publish matrix so it ships the same GitHub-built way. Don't reintroduce a local build +
  push for it as a side channel.

## Operating a CronJob

```bash
kubectl -n navigator get cronjob                                   # schedules + last-run times
kubectl -n navigator create job --from=cronjob/<name> <name>-manual-001   # fire one now
kubectl -n navigator logs job/<name>-manual-001                       # read its output
kubectl -n navigator patch cronjob <name> -p '{"spec":{"suspend":true}}'  # pause without deleting
```

`create job --from` is the standard "run it now to test" affordance — that is how the nightly Archives path was verified
end-to-end after deploy.

## Adding a new scheduled job — checklist

1. Decide the flavor: durable-workflow trigger (A) only if a mid-run crash loses/duplicates something irreversible;
   otherwise self-contained batch (B).
2. Write the Rust binary; **make a re-run safe** — the schedule is at-least-once, and a failed run just runs again next
   period. Exit non-zero on failure so the Job is marked failed and shows in history.
3. Add a server `images/Containerfile.<name>` (or a `--build-arg CRATE=`/`BIN=` row for a trigger), and add the image to
   `deploy.yml`'s publish matrix so CI builds and pushes it to Artifact Registry. (There is no per-image CLI build
   command; CI owns image builds.)
4. Add `cron-<name>.yaml` under `examples/deploy/k8s/exports/` with placeholders, namespace `navigator`, a UTC schedule
   with a Pacific comment.
5. Once CI has published the image, render the manifest to the `YY.M.D` tag and apply (above). For flavor A, also
   re-register the worker — see [durable-workflows.md](durable-workflows.md#the-registration-gotcha).
6. Verify with `create job --from` before trusting the schedule.

## See also

- [Durable workflows](durable-workflows.md) — flavor A, the Restate execution engine, and the registration gotcha. [GKE
  production](gke-prod.md) and [cloud operations](cloud-operations.md) — the cluster + image-shipping mechanics.
  [Deployment secrets](deployment-secrets.md) — where `navigator-web-secrets` values come from.
