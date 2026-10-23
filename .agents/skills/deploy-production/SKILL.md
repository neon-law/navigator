---
name: deploy-production
description: >-
  Roll the neon-law-prod deployment onto a Navigator release tag by hand and verify it, or verify production is wholly
  on a tag without changing anything. Use when the user wants production updated, promoted, rolled back, restarted after
  a secret change, or recovered outside release-deploy.yml — "now do prod", "promote that to production", "roll
  www.neonlaw.com back". This row serves real client matters, so staging must already serve the same tag.
---

# Roll production

Roll `neon-law-prod` onto a release tag. The tag is the argument. This row serves `www.neonlaw.com` and holds real
client matters, so the bar for touching it is higher than for staging in the ways set out below.

`.github/workflows/release-deploy.yml` rolls every stable tag to production unattended once staging passes, and holds
every prerelease tag at staging. Use this skill when a person rolls instead. Check first whether a `release-deploy` run
is in flight (`gh run list --workflow release-deploy.yml --limit 5`); never roll by hand beside one.

Follow [`docs/manual-roll.md`](../../../docs/manual-roll.md) with `ROW=neon-law-prod` and `ROW_RESTATE_ENV=production`.
What follows is specific to this row.

## Staging goes first

Confirm staging already serves this exact tag before the first mutating command:

```bash
./.agents/skills/deploy-staging/scripts/verify-row.sh neon-law-stg <tag>
```

If it does not, say so and stop. The exception is a rollback or an emergency where the user has explicitly weighed this
and said to proceed; note that in the report.

## Review this row's offline plan

Some manifest changes render only for production, so staging clearing a tag says nothing about them. Read this row's
offline operation summary and inspect its source manifests before asking for a yes. The plan does not diff the live
cluster or prove image, Secret, IAM, asset, or endpoint readiness; complete the authenticated checks in
`docs/manual-roll.md` before a real roll.

## Confirm before the first mutation

The offline dry-run, validators, image check, and verification script perform no writes; run them freely. The offline
plan itself makes no cloud calls. Before the first production change, state its tag, the two Deployments, the CronJobs,
and the outgoing tag; then get a clear yes. Staging authorization does not cover production, and approval for one roll
does not cover a later roll.

## This row holds real files

`NAVIGATOR_SIMULATED_MATTERS = "false"` here, and a missing value reads as `false` under the `production` profile. Never
set it `true`: that would seed invented clients among real ones. The archive bucket, telemetry bucket, KMS key, GitHub
App, and Restate environment are all this row's own; never copy a coordinate between rows.

## Verify

```bash
./.agents/skills/deploy-staging/scripts/verify-row.sh neon-law-prod <tag>
```

Capture the outgoing tag before starting; rolling back is `ops ship --tag <outgoing tag>`.

## Finishing

Report both rows and their tags together, along with anything outstanding. If Restate re-registration was judged
current, say it was compared and found current. Remove the `neon-law-prod` kubectl context as `docs/manual-roll.md` step
10 describes.
