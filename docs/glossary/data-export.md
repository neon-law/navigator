---
title: "Data Export"
description: "A Data Export snapshots PostgreSQL tables as Parquet and Iceberg metadata for BigQuery."
---

A snapshot of PostgreSQL tables, written to Parquet and Iceberg metadata on a dedicated GCS bucket. BigQuery reads the
snapshot through BigLake external tables. The [archives crate](../../archives/) owns the writer.

PostgreSQL exports use the [Iceberg Archive](iceberg-archive.md) coverage contract for Navigator and Forgejo. Both
databases must finish before a paired manifest is published. Each records its own watermark and source column types.
Sequence state is outside MVCC. Cross-database atomic recovery and Git, LFS and document bytes have separate recovery
procedures.
