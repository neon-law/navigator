---
title: "Sent Email Export"
description: >-
  The daily copy of each closed day of Sent Email rows to an Iceberg table, and the ledger that lets PostgreSQL purge
  them after a year.
---

The Sent Email Export copies every closed UTC day of [Sent Email](sent-email.md) rows from PostgreSQL to an Apache
Iceberg table in the archive bucket, partitioned by day. Each exported day is recorded in the `sent_email_export` ledger
with its row count and a digest of its row ids. A re-run over an unchanged day writes nothing. A day that gained a late
row is exported again.

PostgreSQL keeps Sent Email rows for one year. The retention purge deletes an older day only when the ledger confirms
the export and the day still matches it, and it keeps rows a [Service](service.md) record cites as evidence. The Iceberg
table keeps every day.

This is narrower than an [Iceberg Archive](iceberg-archive.md): it covers one table, adds day by day, and is the record
that outlives the PostgreSQL rows.

- Schema: [`sent_email_export`](../../store/postgres/schema.sql)
- Workflow: [`archives::sent_email_export`](../../archives/src/sent_email_export.rs)
- Pipeline: [the email events pipeline](../email-events-pipeline.md#the-request-side-store)
