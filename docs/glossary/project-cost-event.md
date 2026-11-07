---
title: "Project Cost Event"
description: "An immutable audit entry for a change to a Project Cost."
---

A Project Cost Event records the [Project Cost](project-cost.md), actor, event kind, and the before and after row
values. The database writes this history alongside governed cost changes.

The table is `project_cost_events`.

Schema: [`project_cost_events`](../../store/postgres/schema.sql).
