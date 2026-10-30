---
title: "Email Token"
description: >-
  An internal one-use proof of control of an email address.
---

Internal plumbing for [Person](person.md) password reset and email confirmation. A token has an expiry and records use
so a consumed link cannot be used again.

The table is `email_token`.

Schema: [`email_token`](../../store/src/schema/navigator.surql#L1294).
