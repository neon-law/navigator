---
title: "Stripe Invoice"
description: An invoice draft created in the Stripe account configured for a Firm and billing destination.
---

Navigator creates an unsent Stripe draft from a [Matter Billing Batch](matter-billing-batch.md). Nevada
[Projects](project.md) use the Firm's Nevada [IOLTA Account](iolta-account.md) payment route. The Firm's operating
Stripe route uses an operating account and carries an explicit disclosure.

The batch retains the Stripe account and draft invoice identifiers so a retry keeps the same destination. The Time
Entries and Project Costs stay reserved for reconciliation.
