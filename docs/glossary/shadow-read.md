---
title: "Shadow Read"
description: >-
  A Shadow Read compares an alternative query with the query that answers a caller.
---

A Shadow Read runs a second query for comparison while one implementation remains authoritative. Navigator's application
reads use PostgreSQL directly, without a sampled SurrealDB query. Captured migration parity checks run through explicit
operator commands, separate from application requests. [PostgreSQL tooling](../storage-models.md#historical-admission).
