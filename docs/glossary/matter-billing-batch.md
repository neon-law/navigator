---
title: Matter Billing Batch
description: One saved invoice request and the matter work reserved for it, retained across retries.
---

## Matter Billing Batch

A **Matter Billing Batch** freezes the billed [Person](person.md)'s name and email, the invoice lines, and the [Time
Entries](time-entry.md) and [Project Costs](project-cost.md) they consume before calling the billing provider. Its
identifier is the provider's idempotency key. Reserved work cannot be written off or linked manually while the batch is
pending. A retry resumes the same request and any saved [Xero Invoice](xero-invoice.md) identifier; later work creates a
new batch after completion.

Xero retains an idempotency key for six minutes from its first call. A batch records its first attempt time once;
retries preserve that time and the same request. An uncertain attempt outside the safe retry window requires manual
reconciliation before another provider create. See [Xero
idempotency](https://developer.xero.com/documentation/guides/idempotent-requests/idempotency/).

Reserved work stays in [Work In Progress](work-in-progress.md) until its invoice appears in the local mirror.

The table is `matter_billing_batch`.

Schema: [`matter_billing_batch`](../../store/postgres/schema.sql).

Vocabulary: [`store::billing_batches`](../../store/src/billing_batches.rs).
