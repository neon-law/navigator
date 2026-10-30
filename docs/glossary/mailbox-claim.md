---
title: "Mailbox Claim"
description: >-
  An internal claim that reserves an email address for one Person.
---

Internal plumbing for [Person](person.md) identity. Writers claiming the same normalized mailbox contend on this one
record, so concurrent requests cannot create rival people for that mailbox.

The table is `person_mailbox`.

Schema: [`person_mailbox`](../../store/src/schema/navigator.surql#L641).
