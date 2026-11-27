---
title: "Standing Data Store"
description: "The Standing Data Store is where Navigator’s rows live."
---

Where Navigator's persistent rows live. PostgreSQL is the relational model for new storage work. SQLx supplies the
connection pool and timestamped migrations; Apache AGE supplies relationship graph traversal. Use the supported
PostgreSQL write path for a domain whose authority is configured there.

PostgreSQL owns the stored state. See [Storage models and writes](../storage-models.md).

- Model: [`store/postgres/schema.sql`](../../store/postgres/schema.sql)
- Migrations: [`store/postgres/migrations`](../../store/postgres/migrations/)
- Access: [`store::postgres`](../../store/src/postgres.rs)
