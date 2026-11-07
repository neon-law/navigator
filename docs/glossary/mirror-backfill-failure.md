---
title: "Mirror Backfill Failure"
description: "A sanitized record of one row a Mirror Backfill could not copy."
---

A failure names its [Mirror Backfill](mirror-backfill.md) run, mirror group, source table, optional row identifier, and
sanitized reason. It records the failure without copying the source row's values.

The table is `mirror_backfill_failure`.

Schema: [`mirror_backfill_failure`](../../store/postgres/schema.sql).
