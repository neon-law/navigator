---
title: "Case Stage Event"
description: "An append-only record of a Project moving from one Case Stage to another."
---

A Case Stage Event names the [Project](project.md), its prior and next [Case Stages](case-stage.md), the Person who
moved it, the time, and an optional note. The newest event determines the Project's current stage.

The table is `stage_event`.

Schema: [`stage_event`](../../store/postgres/schema.sql).
