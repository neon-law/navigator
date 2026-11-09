---
title: "Project Status Transition"
description: "A recorded change to a Project's lifecycle status."
---

A [Project](project.md) lifecycle entry recording the previous and resulting status, the effective and recorded times,
the reason, and the attesting [Person](person.md) when known. PostgreSQL commits the append and the Project's current
status, close date, and closure reason together. An entry with no previous status is the earliest known state. The
ledger keeps earlier closures when the Project reopens and records date corrections as new entries.

`navigator project status` shows entries in recording order. An effective time may be backdated, so it does not
determine the order of entries. Historical facts with no source record remain unset.

The table is `project_status_transition`. See the
[PostgreSQL migration](../../store/postgres/migrations/20261109120000_add_project_status_transition.sql).
