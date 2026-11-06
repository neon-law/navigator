---
title: "Workflow Request"
description: "An internal record binding one durable workflow identifier to its request fingerprint."
---

A Workflow Request records a DBOS workflow identifier, workflow name, request fingerprint, and insertion time. It lets
the durable runtime recognize the request associated with that identifier; it grants no Project access.

The table is `dbos_workflow_request`.

Schema: [`dbos_workflow_request`](../../store/postgres/schema.sql).
