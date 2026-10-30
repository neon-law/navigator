---
title: "Schema Version"
description: >-
  An internal marker of the schema version applied to the standing data store.
---

Internal plumbing for the [Standing Data Store](standing-data-store.md). Startup reads this marker to decide which
schema backfills must run.

The table is `schema_version`.

Schema: [`schema_version`](../../store/src/schema/navigator.surql#L31).
