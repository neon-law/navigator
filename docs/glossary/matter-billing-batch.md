---
title: Matter Billing Batch
description: One saved invoice request and the matter work reserved for it, retained across retries.
---

## Matter Billing Batch

A **Matter Billing Batch** freezes the billed [Person](person.md)'s resolved provider contact identifier, name, and
email; the invoice lines; and the [Time Entries](time-entry.md) and [Project Costs](project-cost.md) they consume before
calling the billing provider. Its identifier is the provider's idempotency key. The invoice uses only the saved contact
identifier for its debtor. Reserved work cannot be written off or linked manually while the batch is pending. A retry
resumes the same request and any saved [Xero Invoice](xero-invoice.md) identifier; later work creates a new batch after
completion.

Matter invoices use the batch's original UTC creation day as their explicit issue date. The due date is 14 calendar days
later and is saved with the batch, so a retry after midnight or a later policy change cannot alter the request. Provider
date corrections and cleared due dates are mirrored from a fresh read.

Xero retains an idempotency key for six minutes from its first call. A batch records its first attempt time once;
retries preserve that time and the same request. An uncertain attempt outside the safe retry window requires manual
reconciliation before another provider create. See [Xero
idempotency](https://developer.xero.com/documentation/guides/idempotent-requests/idempotency/).

Reserved work stays in [Work In Progress](work-in-progress.md) until its invoice appears in the local mirror.

Invoice lines are in USD and exclusive of tax. Xero applies the selected account's configured tax rate. The local mirror
uses the provider's actual total, currency, and dates.

Purchase and sandbox overage invoice creation use separate legacy paths. Those paths do not persist first-attempt time;
an interruption after provider creation can outlive key retention before a retry. The matter-batch guards here do not
cover that recovery risk.

PostgreSQL table: `matter_billing_batch`.

Schema: [`matter_billing_batch`](../../store/postgres/schema.sql).

Vocabulary: [`store::billing_batches`](../../store/src/billing_batches.rs).
