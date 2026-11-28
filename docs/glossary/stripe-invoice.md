---
title: "Stripe Invoice"
description: An invoice draft created in the Stripe account configured for a Firm and billing destination.
---

Stripe is the only invoice issuer. Navigator creates an unsent Stripe draft from a [Matter Billing
Batch](matter-billing-batch.md), or from the lines, discounts, write-offs, credits, and promotion code a lawyer gives
`navigator project invoice create`. Each Firm maps a Project's governing state to its own reviewed trust destination; a
matter without one may use the Firm's Nevada IOLTA where the Firm has confirmed it is permitted. A missing route never
selects Operating: only an [Invoice Destination Approval](invoice-destination-approval.md) does. The draft retains the
state, Stripe account, and invoice identifiers so that a retry keeps its destination, and becomes a [Matter
Invoice](matter-invoice.md). It reaches the client only through `navigator project invoice send`. Time Entries and
Project Costs stay reserved for reconciliation. Before sending a draft, the lawyer verifies the fee agreement, ownership
of the payment, the bank account receiving the payout, and how processor fees and reversals are funded. A Stripe account
identifier alone does not establish those facts.

Run the two-account provider test with `cargo test -p billing two_account_invoice_lifecycle -- --nocapture`. It reads
`STRIPE_SANDBOX_NEVADA_SECRET_KEY` and `STRIPE_SANDBOX_OPERATING_SECRET_KEY` from the environment. Each must be an
`sk_test_` key for a distinct account. The test skips when a key is absent and rejects malformed keys before making
requests. Each run creates isolated test-clock customers, checks invoice ownership and balances, and removes its clocks
and coupon.
