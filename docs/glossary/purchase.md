---
title: "Purchase"
description: "A Purchase is a Client's order of one Offering, activated after provider-verified payment."
---

A Client's order of one [Offering](offering.md) at its published price. Navigator records the Purchase in PostgreSQL
before calling a provider. A consultation raises an `AUTHORISED` Xero invoice keyed on the Purchase id. A recurring
Offering uses Stripe Checkout, retaining the accepted terms, consent time, Firm, and account on the Purchase. Retries
reuse that Purchase's provider key. A Purchase becomes `active` after server-side verification of payment. Stripe access
also requires an unexpired provider-confirmed paid-through date. See [privacy and recurring
purchases](../privacy-and-purchases.md) for cancellation and provider boundaries.

A Purchase is `complimentary`, `invoicing`, `invoiced`, `active`, or `expired`. An expired, unpaid Stripe Checkout
retains its consent record and releases the reservation so the Client can try again. A complimentary Purchase is a
[Consultation](consultation.md) waived for a Client who signed in with Google; it never reaches Xero. The Offering's
name, price, cadence, and account code are copied onto the row, so a later price change never rewrites what a Client was
invoiced.

Purchases are self-service orders. Matter billing follows its own invoice and lawyer-review workflow.

- Table: [`purchase`](../../store/postgres/migrations/20261020120000_create_purchase.sql) Queries:
  [`store::purchases`](../../store/src/purchases.rs) Door: [`portal::purchases`](../../portal/src/purchases.rs)
- Activation: the DBOS `PurchaseActivation` schedule,
  [`purchase_activation`](../../workflows/src/dbos_engine/purchase_activation.rs)

The table is `purchase`.
