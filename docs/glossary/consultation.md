---
title: "Consultation"
description: "A Consultation is a meeting with a lawyer that a Client books, free with a matching Google sign-in."
---

A meeting with a lawyer about a person's situation. A Consultation does not make anyone a client of the firm.

It is an [Offering](offering.md) with one rule of its own: it is complimentary, and booking it optional, for a Client
whose session came through Google with the address on their [Person](person.md), which is the address they confirmed as
a [Lead](lead.md). Anyone else buys it, and books it once the invoice is paid. A session opened by a lead's email
confirmation link does not earn the complimentary Consultation. `/consultation` redirects to the brand's booking page.

- Rule: [`portal::purchases::complimentary`](../../portal/src/purchases.rs)
