---
title: "Project Slack Delivery"
description: "A Project Slack Delivery tracks posting evidence for one Project event."
---

A Project Slack Delivery tracks posting evidence for one [Project](project.md) event. A main-push notice names its
commit and run attempt; a client-view notice names the Project's single client-view event. Native execution checks these
records before posting, including records transferred through a verified object handover.

A channel's existence does not prove that a notice was posted. Unresolved posting evidence holds the client-view event
until reconciliation. A transferred main-push history preserves the source's bounded 200-event retention contract.

The table is `project_slack_delivery`.

Schema: [`project_slack_delivery`](../../store/postgres/schema.sql).
