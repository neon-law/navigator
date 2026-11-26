---
title: "Entity"
description: "An Entity is a legal organization such as an LLC, trust, corporation, or foundation."
---

A legal organization — an LLC, trust, corporation, foundation, etc. Has a name, an [Entity Type](entity-type.md), and a
[Jurisdiction](jurisdiction.md) it is organized under.

- Schema and queries: [`store::entities`](../../store/src/entities.rs) — Lives in: `entity` table.
  Its `entity_type_id` and `jurisdiction_id` are foreign keys. PostgreSQL protects the firm's own row from forking with
  the partial unique `entity_firm_anchor_key` index. Explicit Source compatibility writes claim the same normalized key
  in `firm_anchor`; that claim serializes Source writers whose unique index does not serialize concurrent inserts.

The table is `entity`.

Schema: [`entity`](../../store/postgres/schema.sql).

Supplemental attributes live in the Entity's optional `metadata` JSON object, including an employer identification
number as the string `ein` in `NN-NNNNNNN` format. This is part of the Entity row, not a separate table. The
authenticated `PATCH /app/api/entities/{id}` accepts `metadata`: omitting it preserves the stored object, supplying an
object replaces it, and `{}` clears it. Other Entity edits preserve metadata. Client identifiers belong in Navigator's
private records. Approved public firm facts may be included in the canonical seed. The firm overview displays its linked
Entity's EIN beneath the firm name.

`navigator site import entity` and `PATCH /app/api/entities/{id}` accept `phone`, `url`, and `metadata`. For PATCH and
seed overwrite, omit a contact field to preserve it or supply a blank string to clear it.
