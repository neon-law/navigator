---
title: "Jurisdiction"
description: >-
  A Jurisdiction is a US state, federal jurisdiction, or foreign jurisdiction used to organize an Entity or issue a
  Credential, or Global for a Template written for no single body of law.
---

A US state, federal jurisdiction, or foreign jurisdiction that an Entity can be organized under, or that a
[Credential](credential.md) is issued by. Identified by short code (`NV`, `CA`, `US`, …).

Every notation Template declares one in its `jurisdiction:` frontmatter; rule F110 refuses a Template without it. A
Template written to work under any body of law declares `GLOBAL`, the one row of type `global`.

- Queries: [`store::jurisdictions`](../../store/src/jurisdictions.rs) Schema:
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql) Seed:
  [`store/seeds/Jurisdiction.yaml`](../../store/seeds/Jurisdiction.yaml)

The table is `jurisdiction`.

Schema: [`jurisdiction`](../../store/postgres/schema.sql).
