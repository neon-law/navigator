---
title: "Person Phone"
description: "A Person Phone is one of a Person's phone numbers, normalized to E.164 with a label and a primary flag."
---

A single phone number belonging to a [Person](person.md), stored as one row in the `person_phone` table. The number is
normalized to E.164 — a leading `+`, then 8–15 digits — at the write boundary by `store::person_phones::normalize_e164`,
so every reader, including the Google Contacts sync, sees one spelling. A bare number without a country code is read as
North American (ten digits, or eleven opening with the US country code); anything else without a `+` is refused rather
than guessed at.

`label` is a closed vocabulary — `mobile`, `work`, `home`, `fax`, `other` — held by an ASSERT on the field, the same
shape `person.role` uses. `is_primary` marks the one number a filing or a callback uses first: at most one row per
Person carries it, enforced by store convention (`store::person_phones::set_phone` demotes the others in the same
transaction and is the only writer), because a SurrealDB UNIQUE index cannot express "one per person" over a subset of
rows. The first number a Person gets is primary whether or not the caller said so.

This is a SurrealDB table and [`store::person_phones`](../../store/src/person_phones.rs) is the only module that reads
or writes it; it is mirrored whole-row to PostgreSQL through the `source_domain_records` mirror. The legacy free-text
`person.phone` column on the Person row itself stays what it was for older surfaces; `seed_from_legacy_phone` lifts that
single value into the structured list — as the primary mobile — the first time a Person's phones are updated, guarded so
a Person with structured rows or an unnormalizable legacy value is left alone rather than silently migrated.

The table is `person_phone`.
