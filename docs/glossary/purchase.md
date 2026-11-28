---
title: "Purchase"
description: "A Purchase is a Client's order of one Offering, activated after provider-verified payment."
---

A Client's order of one [Offering](offering.md) at its published price. Navigator records the Purchase in PostgreSQL
before calling a provider. Every paid Purchase collects through the Firm's Stripe Operating account, bound to that Firm
and account. A consultation is a sent Stripe invoice keyed on the Purchase id; its hosted page is the pay link. A
recurring Offering uses Stripe Checkout, retaining the accepted terms and consent time on the Purchase. Retries reuse
that Purchase's provider key. A Purchase becomes `active` after server-side verification of payment. Stripe access also
requires an unexpired provider-confirmed paid-through date. See [privacy and recurring
purchases](../privacy-and-purchases.md) for cancellation and provider boundaries.

A Purchase is `complimentary`, `invoicing`, `invoiced`, `active`, or `expired`. An expired, unpaid Stripe Checkout
retains its consent record and releases the reservation so the Client can try again. A complimentary Purchase is a
[Consultation](consultation.md) waived for a Client who signed in with Google; it never reaches Stripe. The Offering's
name, price, cadence, and account code are copied onto the row, so a later price change never rewrites what a Client was
invoiced.

Purchases are self-service orders. Matter billing follows its own invoice and lawyer-review workflow. A Purchase Xero
invoiced before Stripe keeps its Xero ids as read-only history (`legacy_xero_invoice_id`,
`legacy_xero_repeating_invoice_id`); an unpaid one is expired, and the Firm reconciles it in Xero.

- Table: [`purchase`](../../store/postgres/migrations/20261020120000_create_purchase.sql) Queries:
  [`store::purchases`](../../store/src/purchases.rs) Door: [`portal::purchases`](../../portal/src/purchases.rs)
- Activation: the DBOS `PurchaseActivation` schedule,
  [`purchase_activation`](../../workflows/src/dbos_engine/purchase_activation.rs)

The table is `purchase`.
