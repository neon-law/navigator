---
title: "Matter Invoice"
description: >-
  One invoice on a Project's ledger: a Stripe invoice Navigator drafted, or money collected outside Stripe, with its
  payments.
---

One invoice for a [Project](project.md). Stripe is the only invoice issuer: Navigator composes a draft [Stripe
Invoice](stripe-invoice.md) from a [Matter Billing Batch](matter-billing-batch.md) or from lines a lawyer writes with
`navigator project invoice create`, and the lawyer reviews it before `navigator project invoice send`. Money collected
outside Stripe (in person, by check, by wire, or otherwise) is recorded on the same ledger with its source and external
id.

Each invoice carries its status (`pending`, `draft`, `sent`, `paid`, `void`, or `uncollectible`), its amount, the
destination its payment lands in (the matter's trust account, or the Firm's Operating account by a lawyer's recorded
[Invoice Destination Approval](invoice-destination-approval.md)), and the reason it was voided or marked uncollectible.
Payments are immutable rows in `matter_invoice_payment`; an invoice is paid once they cover it, and a partial payment
leaves a balance. Invoices Xero issued before it was retired are kept as history with source `xero`.

A discount, a write-off to $0, and a credit for a prior payment are lines with their reason, so the client sees the
value and the waiver. A zero-balance invoice is finalized as a record with nothing to pay.

The table is `matter_invoice`.

The table is `matter_invoice_payment`.

Schema: [`matter_invoice`](../../store/postgres/schema.sql) and
[`matter_invoice_payment`](../../store/postgres/schema.sql).

Vocabulary: [`store::matter_invoices`](../../store/src/matter_invoices.rs).
