---
title: "Stripe Customer"
description: >-
  The Stripe Customer a billed Person has on one of the Firm's Stripe accounts, reused for every invoice there.
---

When Navigator drafts a [Stripe Invoice](stripe-invoice.md), it bills the matter's client [Person](person.md) as a
Stripe Customer on the account the invoice collects into. The Customer's id is saved per Firm, Stripe account, and
Person, so the next invoice on that account reuses it; a Customer deleted in Stripe is replaced and the new id saved.

The table is `stripe_customer`.

Schema: [`stripe_customer`](../../store/postgres/schema.sql).

Vocabulary: [`store::stripe_customers`](../../store/src/stripe_customers.rs).
