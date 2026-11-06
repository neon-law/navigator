---
title: "Retention Purge"
description: "The governed removal of a Document chain after its retention window."
---

A Retention Purge records the [Project](project.md), chain, retention cutoff, authorizing Person, start and finish
times, and any expunge record. Its Project, chain, and cutoff identify the operation so a retry resumes the same purge.

The table is `retention_purge`.

Schema: [`retention_purge`](../../store/postgres/schema.sql).
