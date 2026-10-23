---
name: deploy-staging
description: >-
  Roll the neon-law-stg deployment onto a Navigator release tag by hand and verify it, or verify staging is wholly on a
  tag without changing anything. Use when the user wants staging updated, rolled, re-rolled, restarted after a secret
  change, or recovered outside release-deploy.yml — "push the hotfix to staging", "put staging on 26.9.30", "verify
  staging". Staging goes first; for both rows, run this, verify it, then use deploy-production.
---

# Roll staging

Roll `neon-law-stg` onto a release tag. The tag is the argument, for example `26.9.30-rc.1`. If none was given, ask;
never infer it from today's date.

`.github/workflows/release-deploy.yml` rolls every publishable tag to staging. Use this skill when a person rolls
instead: a recovery, a re-roll, a secret-only restart, or while `DEPLOY_AUTOMATION_ENABLED` is unset. Check first
whether a `release-deploy` run for this tag is in flight (`gh run list --workflow release-deploy.yml --limit 5`); never
roll by hand beside one.

Follow [`docs/manual-roll.md`](../../../docs/manual-roll.md) with `ROW=neon-law-stg` and `ROW_RESTATE_ENV=staging`. It
carries the whole sequence: sessions, grants, tag choice, the tagged CLI, the validators, the image check, the offline
plan, Restate, verification, rollback, and cleanup. The offline plan renders local manifests only; it does not check
live drift, cloud authentication, image availability, Secrets, assets, Restate, or public endpoints. What follows is
specific to this row.

## Why staging first

Both rows run the same binary under the `production` runtime profile, so a clean roll here is real evidence about
production. `NAVIGATOR_SIMULATED_MATTERS = "true"` on this row: it seeds only synthetic matters, which is what makes it
the right place to discover a bad tag.

## Verify

```bash
./.agents/skills/deploy-staging/scripts/verify-row.sh neon-law-stg <tag>
```

Read every line rather than the exit status alone. Capture the outgoing tag before you start.

## When release-deploy fails at `ops ship`

The workflow runs `ops ship --quiet`, so the Actions log says only `ship FAILED (exit N) — id <run>-<attempt> …
transcript saved to the private sink`. The real error is in that transcript. Read it with the gcloud CLI: ask the user
to authenticate it (`gcloud auth login`) if it is not already, pin the staging project for the shell, and never run
`gcloud config set`.

```bash
gsutil ls -r gs://<sink-bucket>/ship/ | grep <run-id>
gsutil cat gs://<sink-bucket>/ship/<row>/<run-id>-<attempt>.log | tail -40
```

The bucket is the `STAGING_SHIP_LOG_SINK` secret on the `staging-deploy` environment; when it is not to hand, `gsutil
ls` the staging project's buckets and look for the one holding `ship/`. The last `Error:` line is the cause. Keep
project and bucket names out of PRs, issues, and commits.

## When staging looks wrong

Two things that look like regressions are not:

- **The webapp bundle answers `303` to `/auth/login`.** The bundle sits behind the session boundary, so an anonymous
  request is redirected.
- **A brief load-balancer warning during the roll**, such as `endpoint count cannot be zero`, is the NEG swapping
  endpoints while pods replace, not a failure.

`workflows-service` runs `maxSurge: 0`, so while the surge web pod comes up it can sit `Pending`. If the autoscaler
reports `FailedScaleUp: GCE quota exceeded`, read the regional quota before anything else:

```bash
gcloud compute regions describe us-west4
```

The worker schedules once the old web pod finishes terminating, well inside `ops ship`'s rollout wait.

Before rolling back, run the same verification against the tag that was running. If the symptom reproduces there, the
tag is not the cause and a rollback will not fix it.

## Finishing

Report which row is on which tag, what verified, and anything left outstanding, then remove the `neon-law-stg` kubectl
context as `docs/manual-roll.md` step 10 describes.
