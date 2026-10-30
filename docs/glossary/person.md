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

This is a SurrealDB table and [`store::persons`](../../store/src/persons.rs) is the only module that reads or writes it.
Every `person_id` on another table is therefore an unenforced cross-engine id, resolved in Rust. A [Lead](lead.md) is a
public contact request, not a second directory: conversion and linking write this table and point `lead.person_id` at
the row.

One Person per mailbox is protected from forking by a claim in the `person_mailbox` table, whose record id is the
lowercased email, rather than by the UNIQUE `person_email_lower` index alone. The index is the backstop behind it — it
refuses a fork that is not a race, but racers write no shared key for the engine to conflict on, so the claim is what
serializes them. It matters here more than elsewhere because `role` is the authorization root: a forked mailbox is one
human carrying two roles. The claim moves when an edit moves the email and is released when the Person is deleted, so a
mailbox is reusable rather than locked out.

**One human, several addresses.** A second claim for the same Person is an *alias*: an attorney who changes firms keeps
the old address, and `store::persons::find_by_any_email` finds the row by either. No new table holds aliases — they are
more `person_mailbox` rows, so they are exactly as unique as a primary email and are mirrored to PostgreSQL by the same
`person_mailbox` mirror. `site import person` reads `previous_email:` and `emails:` on a record: a Person who answers to
any of them is updated to the record's `email` instead of duplicated, and the rest are kept as aliases. When a duplicate
already exists, `navigator site person merge <keep> <drop>` ([`store::person_merge`](../../store/src/person_merge.rs))
moves the dropped client Person's relationships — Conflict-Check Graph edges, Project participation, Relationship Log
rows — to the kept Person in one transaction, hands over both addresses, and deletes the duplicate last. Aliases are
written through the SurrealDB claim only; they are refused under PostgreSQL People authority until that path lands.

- Schema: [`person` in `navigator.surql`](../../store/src/schema/navigator.surql) Queries:
  [`store::persons`](../../store/src/persons.rs)

The table is `person`.

Schema: [`person`](../../store/src/schema/navigator.surql#L584).
