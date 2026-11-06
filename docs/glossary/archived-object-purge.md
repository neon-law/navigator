---
title: "Archived Object Purge"
description: "A record that retention purge removed one archived object from a Project."
---

The [Retention Purge](retention-purge.md) records the Project, content digest, removal time, and any expunge record. A
[Project Recovery Archive](project-recovery-archive.md) uses this record to distinguish removed bytes from corruption.

The table is `archived_object_purge`.

Schema: [`archived_object_purge`](../../store/postgres/schema.sql).
