---
title: "Task Event"
description: "An immutable audit entry for a Task's creation, assignment, or state change."
---

Each event names the [Task](task.md), actor, event kind, and before and after values. Task changes retain this history
instead of removing a completed Task or silently replacing its assignment.

The table is `project_task_events`.

Schema: [`project_task_events`](../../store/postgres/schema.sql).
