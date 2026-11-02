---
title: "Entity Type"
description: "An Entity Type names the kind of legal Entity, such as an LLC, trust, corporation, or foundation."
---

The kind of legal Entity (`LLC`, `Trust`, `Corporation`, `Foundation`, …). Reference data, seeded from
[`store/seeds/EntityType.yaml`](../../store/seeds/EntityType.yaml).

- Schema and queries: [`store::entity_types`](../../store/src/entity_types.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `entity_type`.

Schema: [`entity_type`](../../store/postgres/schema.sql).
