---
title: "Task"
description: "Assignable work with a workflow state — todo, in progress, done — audited apart from the docket."
---
Assignable work on a [Project](project.md): someone does it, it holds a workflow state, and it moves forward — `todo` →
`in_progress` → `done`. A Task is **not** a [Deadline](deadline.md). A Deadline is the firm's docket — a date the law, a
court, or a contract binds, where a miss is a malpractice event. A Task is work the firm chose to do; it may name a
`due_on` date as a scheduling aid, but nothing about the row is an obligation the date binds. They are separate tables,
separate terms, and separate matter-page copy. **State is a transition, never a delete.** A done Task keeps its row. The
only legal moves are one step forward — `todo` → `in_progress` → `done` — and no state removes the row or its history.
Reopening a done Task is not offered in v1; a new Task expresses work that came back. **Assignment is
participant-only.** `assignee_id` names a [Person](person.md) who already holds a participation row on the same Project.
The store refuses an outsider with a typed denial, and the database refuses one with a foreign key — an assignee cannot
be a Person the matter has never seen. **Every mutation is audited.** Create, assignment, and state change each append a
`project_task_event` — the actor, the event kind, and the row before and after. The database itself refuses a write that
carries no audit context, and the event log is immutable. This is the `project_costs` pattern: the table, not the
caller, keeps the record honest. **Storage.** `project_task` is PostgreSQL-native (`store/src/tasks.rs`) — a net-new
table with no Surreal source. The Project must already be mirrored to PostgreSQL before it can hold Tasks. Schema:
`store/postgres/migrations/20261030120000_create_project_task.sql`.
