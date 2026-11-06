---
title: "Schema Version"
description: "A Schema Version identifies an applied database schema change."
---

An applied change to the [Standing Data Store](standing-data-store.md) schema. PostgreSQL's SQLx migration runner
records ordered versions and checksums in `_sqlx_migrations`. Run the explicit migration command before serving
requests; see [PostgreSQL access and migrations](../postgres-tooling.md).

The compatibility engine's version marker is described in [SurrealDB compatibility](../surreal-compatibility.md#schema).

The SurrealDB compatibility table is `schema_version`.
