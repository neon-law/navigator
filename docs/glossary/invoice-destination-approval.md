---
title: "Invoice Destination Approval"
description: >-
  A responsible lawyer's retained approval to collect one invoice into the Firm's Operating account instead of trust.
---

A [Matter Invoice](matter-invoice.md) collects into the matter's state IOLTA, or the Firm's Nevada IOLTA where the Firm
has confirmed that state's rules allow it. A missing trust route never selects Operating on its own. The matter's
responsible lawyer may approve the Firm's Operating account for one invoice of earned work; an advance, a deposit, or an
unearned flat fee always collects into trust.

The approval is the audit record: who approved, when, why, the destination, and the reviewed invoice digest. The invoice
or [Matter Billing Batch](matter-billing-batch.md) that collects under it references it, and every retry stays pinned to
it. It is never edited or deleted.

The table is `invoice_destination_approval`.

Schema: [`invoice_destination_approval`](../../store/postgres/schema.sql).

Vocabulary: [`store::invoice_destinations`](../../store/src/invoice_destinations.rs).
