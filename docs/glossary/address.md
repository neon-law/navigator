---
title: "Address"
description: "An Address is a postal location attached to a Person, an Entity, or neither."
---

A postal address attached to a Person, to an Entity, or to neither (the mailroom placeholder). The person/entity
exclusivity is enforced by the application, not the schema.

- Schema and queries: [`store::addresses`](../../store/src/addresses.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `address`.

Schema: [`address`](../../store/postgres/schema.sql).

`navigator site import address` accepts a Person address with lookup fields `person_id`, `line1`, and `postal_code`. The
record names the owner by `person.email` and gives `line1`, optional `line2`, `city`, `region`, `postal_code`, and
`country`. For an Entity address, declare lookup fields `entity_id`, `street`, and `zip`. Name the owner by
`entity.name` and supply `street`, `city`, `state`, `zip`, and `country`. Include `entity.entity_type.name` when the
Entity name alone is ambiguous. An existing owner, street, and postal code match is left unchanged.
