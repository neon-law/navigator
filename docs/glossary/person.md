---
title: "Person"
description: "A Person is a human contact."
---

A human contact. The system-wide tier — `owner`, `admin`, `lawyer`, `clerk`, or `client` — lives on this row in the
`role` field, not on the OIDC token. The Rauthy / Google id_token carries only `sub` and `email`; the callback handler
links that pair to a Person via the presenting provider's own subject column — `oidc_subject` for the primary slot,
`microsoft_subject`, or `apple_subject` — and reads `role` from the DB. `lawyer` means a person licensed to practice law
authorized for Navigator legal work, not a firm email or source-forge membership. See
[`docs/access-model`](../access-model.md) and [`docs/oidc`](../oidc.md).

The PostgreSQL `person` table holds the model, and [`store::persons`](../../store/src/persons.rs) is its public
accessor. With People authority enabled, Person rows and mailbox claims commit in one PostgreSQL transaction. A
[Lead](lead.md) converts into a Person and records that row's id in `lead.person_id`.

The `person_mailbox` table gives each normalized mailbox one Person. Claims move with email edits and are released on
deletion. Authority selection and compatibility projection are described in [Storage models and
writes](../storage-models.md#people).

**One human, several addresses.** A second claim for the same Person is an *alias*: an attorney who changes firms keeps
the old address, and `store::persons::find_by_any_email` finds the row by either. No new table holds aliases — they are
more `person_mailbox` rows, so they are exactly as unique as a primary email and are mirrored to PostgreSQL by the same
`person_mailbox` mirror. `site import person` reads `previous_email:` and `emails:` on a record: a Person who answers to
any of them is updated to the record's `email` instead of duplicated, and the rest are kept as aliases. When a duplicate
already exists, `navigator site person merge <keep> <drop>` ([`store::person_merge`](../../store/src/person_merge.rs))
moves the dropped client Person's relationships — Conflict-Check Graph edges, Project participation, Relationship Log
rows — to the kept Person in one transaction, hands over both addresses, and deletes the duplicate last. Alias writes
and Person merges require the compatibility path; PostgreSQL People authority refuses them. See [SurrealDB
compatibility](../surreal-compatibility.md#people).

- Schema: [`person` in the PostgreSQL schema](../../store/postgres/schema.sql) Queries:
  [`store::persons`](../../store/src/persons.rs)

The table is `person`.

Schema: [`person`](../../store/postgres/schema.sql).
