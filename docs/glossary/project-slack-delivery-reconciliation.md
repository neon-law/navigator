---
title: "Project Slack Delivery Reconciliation"
description: "A Project Slack Delivery Reconciliation records an attributed posting-evidence decision."
---

A Project Slack Delivery Reconciliation resolves an unresolved client-view notice as posted or verified not-posted.
The privileged operation records its identity, ownership epoch, authorized Person actor and private evidence reference.
Actor attribution does not authenticate an independently supplied capture or prove source quiescence.

The decision and its audit record commit together under the Project lane. Exact operation replay is idempotent;
conflicting decisions refuse. The audit record cannot be updated or deleted and retains its historical Project identity
after canonical Project deletion. Recording the decision sends no provider notice.

The table is `project_slack_delivery_reconciliation`.

Schema: [`project_slack_delivery_reconciliation`](../../store/postgres/schema.sql).
