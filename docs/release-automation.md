# Release automation

The release path uses two workflows. `release-cut.yml` opens a version-only pull request; `release-deploy.yml` rolls
published tags through the configured staging and production deployments. This runbook describes each workflow, its
required identities and settings, and how to enable it.

Read [`gitops.md`](gitops.md) first. `deploy.yml` proves the workspace, cuts the tag, and publishes every image and
archive; the workflows here come before and after it. `cli/tests/release_cut_workflow.rs`,
`cli/tests/release_deploy_workflow.rs`, and `cli/tests/deploy_workflow.rs` read the checked-in workflow files, so every
structural claim below is also a test. If this document and a workflow disagree, the workflow and its tests win.

## What `release-cut.yml` does

Once enabled, a 09:00 UTC schedule opens a pull request that is exactly `ops cut-release`'s output — a version bump, a
refreshed `Cargo.lock`, and swept action pins — nothing else. A `merge` job waits for the required `ci` check, then
re-verifies the PR's actual diff touches only `Cargo.toml`, `Cargo.lock`, or a workflow/action pin file before merging
it with a narrowly scoped release App token (`contents: write` + `pull_requests: write`, no `workflows` permission — the
same shape `docs/gitops.md`'s auto-merge identity callout describes). That merge is an ordinary push to `main`;
`deploy.yml` takes over from there exactly as it does for a human-opened bump.

The release PR follows the same merge policy as every other PR. `production` requires the `ci` check, signed commits,
linear history, resolved threads, and squash-only merging, but no approving review.

`release-cut.yml` runs only while `vars.RELEASE_TRAIN_ENABLED` is `true`, on both of its jobs.

## What `release-deploy.yml` does

After image and CLI/LSP publishing succeeds, the final job dispatches `release-deploy.yml` when automation is enabled.

```bash
gh release download <tag> --pattern navigator-image-digests.json --dir /tmp
gh workflow run release-deploy.yml --ref main -f tag=<tag> \
  -f image-digests="$(cat /tmp/navigator-image-digests.json)"
```

with a token holding only `actions: write`. The dispatch runs on `main`, so the workflow file and the `slack-progress`
action come from `main`. No row comes from a checkout: each ship reads the row's published record from its own project.

`release-deploy.yml` then runs these jobs:

| Job | What it does |
| --- | --- |
| `preflight` | Reads back the gate below, verifies the Release exists, and reads whether it is a prerelease |
| `dedup` | Skips the run when the tag is already the last successful production deployment |
| `resolve-rollback` | On `rollback: true`, confirms the tag is a recorded successful production deployment |
| `ship-staging` | Full `ops ship` onto the configured staging deployment, then the health checks |
| `hold-production` | For a prerelease: posts that the tag was held at staging |
| `ship-production` | Stable tags ship to configured production, then run health checks |
| `notify` | Posts the staging and production outcome |

Each ship job records a GitHub Deployment, installs the signed CLI image by digest and checks `navigator --version`
reports the release tag, and runs:

```bash
navigator ops ship --deployment <row> --project-id <project> --tag <tag> --image-digests /tmp/image-digests.json --assert-signing-iam --quiet --log-sink <sink>
```

The runner checks out no deployment tree. `--project-id` reads the row's published record — the only source of a row,
created with `navigator ops deployments init` or `import` and edited with `set` / `unset` (see
[`manual-roll.md`](manual-roll.md#the-published-record-is-the-row)) — from the `STAGING_GCP_PROJECT_ID` or
`PRODUCTION_GCP_PROJECT_ID` environment secret. A row edit takes effect on the next ship with no merge to `main`. `ops
ship` writes the GKE credentials and a short-lived registry login for `cosign` to its own scratch directory, so neither
depends on the runner's or an operator's shell. The brand-readiness and Restate registration health checks read the same
record. A tag whose CLI predates their `--project-id` flag skips them with a warning; a tag older than `ops ship
--project-id` (26.10.20) must be rolled by hand.

Both ship jobs use quiet logging with each protected environment’s configured sink, keeping full transcripts private.

The CI identity receives `roles/storage.objectCreator` on this deployment's log bucket, scoped to the `ship/` prefix.

It has no read, list, overwrite, or delete access.

Each deployment uses a separate private bucket because transcripts can contain production pod logs.

See `glossary show "Ship Transcript"` for details.

Operators retrieve a run using their own credentials:

```sh
navigator ops logs --deployment <row> --run <run-id>-<attempt> [--prefix <prefix>]
```

That is the full reconcile: manifests, the Secret checks, CronJob pins, and Restate registration. `--assert-signing-iam`
verifies the web service account's signing binding without writing IAM. The health checks are `ops brand-readiness` and
`ops restate register`, and each posts its name, the tag, and `pass` or `fail` to `#navigator`. `ship-production` opens
with a line saying staging passed and production begins.

`ship-production` needs `ship-staging` and carries no status function in its `if:`, so a failed, cancelled, or skipped
staging run blocks it. It rolls `needs.ship-staging.outputs.tag` — exactly the tag staging served.

### Prereleases stay on staging

A tag is a prerelease when `ops release check` reports `prerelease=true` for it — any semver prerelease, such as `-rc.N`
or `-hotfix.N`. `deploy.yml` marks such a Release as a prerelease, and `preflight` reads that flag back and also treats
any tag with a `-` suffix as one. A prerelease rolls to staging; `ship-production` skips, `hold-production` posts that
the tag was held at staging, and no production Deployment is recorded. A hotfix reaches production through
[`manual-roll.md`](manual-roll.md) once staging serves it.

## The gate

`preflight` reports `proceed=true` only when both hold, and every other job needs it:

1. **This is the expected repository.** Set the repository variable `DEPLOY_EXPECTED_REPOSITORY` to its
   `owner/name`; the workflow compares it with `GITHUB_REPOSITORY`.
2. **The run is on `main`.** `GITHUB_REF` is `refs/heads/main`.

The unattended on-switch is `vars.DEPLOY_AUTOMATION_ENABLED`, read in one place: `deploy.yml`'s `release-deploy` job.
Unset, a release publishes and nothing is dispatched. A manual dispatch is the rehearsal, re-roll, and rollback path. To
stop every run at once, including manual dispatches:

```bash
gh workflow disable release-deploy.yml --repo <owner>/<repository>
```

## Identities and settings

Each ship job uses its environment's configured service account for Workload Identity.

Providers accept tokens only from the expected repository and deployment environment on `main`.

Bind each account to the environment principal set with `roles/iam.workloadIdentityUser`. Do not reuse an unscoped
provider.

The account holds what the full `ops ship` uses:

- **`roles/container.clusterViewer`** and **`roles/container.developer`** on the deployment's project. The first
  covers `gcloud container clusters get-credentials`. The second covers `apply` on every kind the manifest renders,
  including the Namespace, plus the reads the rollout waits use. This is the same Kubernetes access a person running
  `ops ship` holds. It includes `container.secrets.get`, so the account can read every Kubernetes Secret value in the
  cluster. A namespaced Kubernetes `Role` would narrow that, and is not written yet.
- **`roles/artifactregistry.reader`** on the configured image repository, for the published-tag check on every image.
- **`roles/storage.objectViewer`** on the row's `NAVIGATOR_ASSETS_BUCKET`, for the public-asset check.
- **`roles/secretmanager.viewer`** on the project, for the enabled-version check. It reads metadata and no payload.
- **`roles/secretmanager.secretAccessor`** on the one `navigator-deployment-config-<row>` secret, for the published
  configuration. It holds coordinates and secret key names, never a credential value.
- **Custom role `navigatorSigningIamReader`** (`iam.serviceAccounts.getIamPolicy`) on the project, for
  `--assert-signing-iam`. It is bound at the project level because a deny policy blocks `setIamPolicy` on the web
  service account for everyone but its admin, so the resource-level binding cannot be written.

It holds no KMS decrypt (`ops ship` reads SOPS key names, never values) and no `setIamPolicy`. Restate registration
authenticates with the admin token below.

The GitHub token each job receives:

| Job | Permissions |
| --- | --- |
| `deploy.yml` `release-deploy` | `actions: write` |
| `preflight`, `hold-production`, `notify` | `contents: read` |
| `dedup`, `resolve-rollback` | `contents: read`, `deployments: read` |
| `ship-staging`, `ship-production` | `contents: read`, `id-token: write`, `deployments: write` |

Each environment holds its own secrets and variables, never a repository-wide copy either ship job could read. The
Restate admin tokens are Restate Cloud API keys, one per environment, not the `restate` CLI's login session, which lasts
about a day:

- **`staging-deploy`**: `STAGING_SHIP_WIF_PROVIDER`, `STAGING_SHIP_SERVICE_ACCOUNT`,
  `STAGING_DEPLOYMENT`, `STAGING_SHIP_LOG_SINK`, and `STAGING_RESTATE_ADMIN_TOKEN` are secrets;
  `STAGING_RESTATE_ADMIN_URL` is a variable.
- **`production-deploy`**: `PRODUCTION_SHIP_WIF_PROVIDER`, `PRODUCTION_SHIP_SERVICE_ACCOUNT`,
  `PRODUCTION_DEPLOYMENT`, `PRODUCTION_SHIP_LOG_SINK`, and `PRODUCTION_RESTATE_ADMIN_TOKEN` are secrets;
  `PRODUCTION_RESTATE_ADMIN_URL` is a variable.

At repository scope, set variable `DEPLOY_EXPECTED_REPOSITORY` and secrets `NAVIGATOR_IMAGE_REGISTRY`,
`NAVIGATOR_REGISTRY_HOST`, `NAVIGATOR_PUBLISHER_SERVICE_ACCOUNT`, and `NAVIGATOR_PUBLISHER_WIF_PROVIDER`.
`SLACK_WEBHOOK_URL` is the repository secret every workflow already posts through. `DEPLOY_AUTOMATION_ENABLED` and
`RELEASE_TRAIN_ENABLED` are repository variables. `release-cut.yml` also reads `vars.RELEASE_APP_ID` and
`secrets.RELEASE_APP_PRIVATE_KEY`.

An unset `DEPLOY_EXPECTED_REPOSITORY` or publisher secret is not a fork. Empty coordinates make `github.repository !=
''` true everywhere, so `deploy.yml` fails a publishable commit in `release-version` rather than running the KIND suite
and then skipping the tag.

## Serialization, duplicate runs, and idempotency

`release-deploy.yml` holds one fixed concurrency group (`release-deploy`, `cancel-in-progress: false`) across every run,
so two tags never roll onto the same clusters at once. `dedup` reads the last GitHub Deployment recorded for
`production`; when its `ref` equals the tag and its last status is `success`, every ship job skips.

## Last-known-good and rollback

The **last-known-good** tag is a GitHub Deployment against `production` whose latest status is `success` — the record
`ship-production` writes on every run, pass or fail. A rollback is a dispatch naming that tag:

```bash
gh release download <last-known-good> --repo <owner>/<repository> --pattern navigator-image-digests.json --dir /tmp
gh workflow run release-deploy.yml --repo <owner>/<repository> --ref main \
  -f tag=<last-known-good> -f rollback=true -f image-digests="$(cat /tmp/navigator-image-digests.json)"
```

`resolve-rollback` fails unless the tag appears among successful production deployments; the tag then rolls through
staging and production like any other. The release digest manifest supplies immutable references; Artifact Registry
retains the ten newest versions per image.

## Turning it on

1. **Provision a least-privilege ship identity per environment.** Create the row's record (`ops deployments init`, or
   `import` once from a private tree), then run setup with the row, repository, and GitHub Environment named explicitly.
   Nothing infers them:

   ```bash
   navigator ops deployments import <row> --deployments-dir <private-root>
   navigator ops gcp setup --project-id <project-id> \
     --ship-deployment <row> --ship-repository <owner>/<repository> --ship-environment <github-environment>
   ```

   Setup is idempotent: a re-run reads every policy and writes nothing that is already in place. For
   `navigator-release-ship` it grants the project roles `container.clusterViewer`, `container.developer`, and
   `secretmanager.viewer`, and the custom `navigatorSigningIamReader` role, which it creates when absent;
   `artifactregistry.reader` on the image repository, the hub's when `--images-project-id` is set;
   `storage.objectViewer` on the assets bucket; and `secretmanager.secretAccessor` on
   `navigator-deployment-config-<row>` alone, creating that secret's shell when absent and adding no version. It creates
   the `navigator-release` pool and its `github` OIDC provider, and binds `iam.workloadIdentityUser` for the
   environment's principal set. It grants no KMS access and no `setIamPolicy`. The provider admits only:

   ```text
   assertion.repository == '<owner>/<repository>' && assertion.environment == '<github-environment>'
     && assertion.ref == 'refs/heads/main'
   ```

   Keep the ship identity out of broad deployment groups. It needs only the permissions listed above.

   Without the published configuration or the grant, `ops ship` fails with `secretmanager.versions.access`
   `PERMISSION_DENIED`. Secret Manager returns 403, not 404, for a secret the caller cannot read, so a secret that was
   never published fails the same way. The ship error names both commands above.

2. **Create `staging-deploy` and `production-deploy`**, each limited to the `main` branch, with a required reviewer
   on production and the secrets and variables above. Set the repository variable `DEPLOY_EXPECTED_REPOSITORY`.
   `*_SHIP_WIF_PROVIDER` is
   `projects/<number>/locations/global/workloadIdentityPools/navigator-release/providers/github`.
3. **Rehearse on staging.** With `DEPLOY_AUTOMATION_ENABLED` still unset, dispatch a published `-rc` tag by hand and
   read the run: staging rolls and passes its health checks, and production is held with no production Deployment
   recorded. Rehearse a failed staging roll and a rollback dispatch the same way.
4. **Set `DEPLOY_AUTOMATION_ENABLED=true`.** The next stable tag `deploy.yml` publishes rolls to staging and then
   production unattended.
5. **Provision the release App** (`contents: write` + `pull_requests: write`, nothing else), set `vars.RELEASE_APP_ID`
   and `secrets.RELEASE_APP_PRIVATE_KEY`, dry-run `release-cut.yml`'s `cut` job by hand, and then set
   `RELEASE_TRAIN_ENABLED=true`.
