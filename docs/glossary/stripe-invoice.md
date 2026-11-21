---
title: "Stripe Invoice"
description: An invoice draft created in the Stripe account configured for a Firm and billing destination.
---

Navigator creates an unsent Stripe draft from a [Matter Billing Batch](matter-billing-batch.md). Each Firm maps a
Project's governing state to its own reviewed trust destination. The batch retains the state, Stripe account, and
invoice identifiers so that a retry keeps its destination. Time Entries and Project Costs stay reserved for
reconciliation.
