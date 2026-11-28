---
title: Matter Billing Batch
description: One saved invoice request and the matter work reserved for it, retained across retries.
---

## Matter Billing Batch

A **Matter Billing Batch** freezes the billed [Person](person.md)'s name and email, the invoice lines, the [Time
Entries](time-entry.md) and [Project Costs](project-cost.md) it reserves, and the payment destination before calling
Stripe. Its identifier supplies every Stripe write's idempotency key (`{batch}/{step}`). Reserved work cannot be written
off or linked manually. A retry resumes the request, the saved [Stripe Invoice](stripe-invoice.md) identifier, and the
destination it was created with: the matter's trust account, or the Operating account under its [Invoice Destination
Approval](invoice-destination-approval.md). A new batch can compose after the current batch completes.

The batch's draft becomes a [Matter Invoice](matter-invoice.md), and its work moves to invoiced under that ledger row.

Matter invoices use the batch's original UTC creation day as their explicit issue date. The due date is 14 calendar days
later and is saved with the batch, so a retry after midnight or a later policy change cannot alter the request.

Stripe keeps an idempotency key for 24 hours. A batch records its first attempt time once; a retry within 23 hours of it
resumes the same draft, and an older uncertain attempt requires reconciliation before another Stripe create. See [Stripe
idempotent requests](https://docs.stripe.com/api/idempotent_requests).

Reserved work stays in [Work In Progress](work-in-progress.md) until its draft is created.

Invoice lines are in USD.

PostgreSQL table: `matter_billing_batch`.

Schema: [`matter_billing_batch`](../../store/postgres/schema.sql).

Vocabulary: [`store::billing_batches`](../../store/src/billing_batches.rs).

The table is `matter_billing_batch`.
