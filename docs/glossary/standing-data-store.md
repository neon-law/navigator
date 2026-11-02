---
title: "Standing Data Store"
description: "The Standing Data Store is where Navigator’s rows live."
---

Where Navigator's persistent rows live. PostgreSQL is the relational model for new storage work. SQLx supplies the
connection pool and timestamped migrations; Apache AGE supplies relationship graph traversal. Use the supported
PostgreSQL write path for a domain whose authority is configured there.

The active backend is selected per domain. A PostgreSQL mirror alone does not make that database authoritative. Read
[Storage models and writes](../storage-models.md) before choosing an accessor or enabling a deployment. Remaining
engine-specific paths are documented in [SurrealDB compatibility](../surreal-compatibility.md).

- Model: [`store/postgres/schema.sql`](../../store/postgres/schema.sql)
- Migrations: [`store/postgres/migrations`](../../store/postgres/migrations/)
- Access: [`store::postgres`](../../store/src/postgres.rs)
