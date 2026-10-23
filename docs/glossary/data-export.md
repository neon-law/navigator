---
title: "Data Export"
description: "A Data Export snapshots SurrealDB and PostgreSQL tables as Parquet and Iceberg metadata for BigQuery."
---

A snapshot of one or more SurrealDB tables, written to Parquet (and Iceberg metadata) on a dedicated GCS bucket,
consumed by BigQuery via BigLake external tables. The [`archives`](../../archives/) crate owns the writer, exposed as
the `Archives` Restate workflow hosted by the `workflows-service` worker (all workflows live there). The
[`cron-archives-trigger.yaml`](../../examples/deploy/k8s/exports/cron-archives-trigger.yaml) CronJob fires at 05:55 and
17:55 UTC each day to start separate invocations; the workflow runs the snapshot (and, when configured, a GCP
cost-by-service summary written as the `gcp_cost` table) as durable steps, then posts snapshot outcomes and promotion
failures through the worker's Slack bot to its messages channel. The scheduled UTC time keys the workflow and selects
one full snapshot per table, so the evening run cannot combine the morning's rows with its own. Duplicate triggers for
the same scheduled time attach to the same durable journal.

Configured PostgreSQL exports use the [Iceberg Archive](iceberg-archive.md) coverage contract for Navigator and Forgejo.
Both databases must finish before a paired manifest is published. Each records its own watermark and source column
types. Sequence state is outside MVCC. Cross-database atomic recovery and Git/LFS/document-byte recovery require
separate tested procedures.

Disambiguates from the deploy **source export** — that ships git bundles of HEAD to `gs://YOUR_PROJECT_ID-source/` for
repo distribution. Two buckets, two flavors of "export," one shared word.

- Crate: [`archives/`](../../archives/) Bucket: the dedicated `NAVIGATOR_ARCHIVES_BUCKET`, under `application/<table>/`.
