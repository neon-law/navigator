---
title: "Purchase"
description: "A Purchase is a Client's order of one Offering, invoiced in Xero and active once paid."
---

A Client's order of one [Offering](offering.md) at its published price. Navigator records the Purchase in PostgreSQL
before calling Xero, then raises an `AUTHORISED` Xero invoice keyed on the Purchase id, so a retry never raises a second
one. Xero emails the invoice and collects payment. The Purchase turns `active` once Xero reports the invoice `PAID`; for
a monthly or yearly Offering, Xero then raises and emails each renewal from a repeating invoice.

A Purchase is `complimentary`, `invoicing`, `invoiced`, or `active`. A complimentary Purchase is a
[Consultation](consultation.md) waived for a Client who signed in with Google; it never reaches Xero. The Offering's
name, price, cadence, and account code are copied onto the row, so a later price change never rewrites what a Client was
invoiced.

This is the one self-serve invoice Navigator raises. Lawyers bill matters in Xero.

- Table: [`purchase`](../../store/postgres/migrations/20261020120000_create_purchase.sql) Queries:
  [`store::purchases`](../../store/src/purchases.rs) Door: [`portal::purchases`](../../portal/src/purchases.rs)
- Activation: the DBOS `PurchaseActivation` schedule,
  [`purchase_activation`](../../workflows/src/dbos_engine/purchase_activation.rs)
