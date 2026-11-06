---
title: "Mirror Backfill Checkpoint"
description: "The saved progress of one mirror group within a Mirror Backfill run."
---

A checkpoint belongs to a [Mirror Backfill](mirror-backfill.md) run and mirror group. It records batch, applied, and
failed counts, timestamps, and completion so the run can resume its recorded work.

The table is `mirror_backfill_checkpoint`.

Schema: [`mirror_backfill_checkpoint`](../../store/postgres/schema.sql).
