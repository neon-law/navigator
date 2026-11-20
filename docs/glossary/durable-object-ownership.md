---
title: "Durable Object Ownership"
description: "Durable Object Ownership records the epoch admitting native object execution."
---

Durable Object Ownership records the single activated epoch for the Notation and Project Slack adapters. It references
one [Durable Object Handover](durable-object-handover.md), distinguishing a verified captured-source transfer from
sanctioned fresh-native initialization. PostgreSQL application authority alone does not establish this ownership.

The adapters require this epoch before serving native object operations. Captured-state activation separately checks
source evidence and target parity; routing and retained-work disposition remain deployment acceptance requirements. An
activated epoch does not permit a different legacy capture to overwrite native progress.

The table is `durable_object_ownership`.

Schema: [`durable_object_ownership`](../../store/postgres/schema.sql).
