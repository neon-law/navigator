---
title: "Project Source Import"
description: "A durable request to initialize a Project repository from a validated source archive."
---

A Project Source Import records the [Project](project.md), Firm, operation key, source-tree digest, and file count.
While pending it retains the canonical archive for retry. A committed request records its repository and verified
commit; a refused request records its outcome. It imports repository source, not legal document bytes.

The table is `project_source_import`.

Schema: [`project_source_import`](../../store/postgres/schema.sql).
