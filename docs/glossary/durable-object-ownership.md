---
title: "Durable Object Ownership"
description: "Durable Object Ownership records the epoch admitting native object execution."
---

Durable Object Ownership records the single PostgreSQL epoch that admits Notation and Project Slack execution through
DBOS. It references a [Durable Object Handover](durable-object-handover.md) receipt. New databases establish the epoch
during provisioning. A Deployment Operator establishes the epoch for an existing database using the no-capture cutover.

The table is `durable_object_ownership`.

Schema: [`durable_object_ownership`](../../store/postgres/schema.sql).
