---
title: "Offering"
description: "An Offering is something a signed-in Client may buy online at a published price."
---

Something a signed-in Client may buy online: a [Consultation](consultation.md), or a Delete Your Data plan billed
monthly or yearly. Each Offering is an entry in the services catalog's `purchase.offerings` block with an id, name,
published price, cadence (`once`, `month`, or `year`), and the Xero account code its revenue posts to. Recurring
Offerings also carry attorney-approved `renewal_terms`, including how to cancel. The purchase form displays those terms
and requires an unchecked agreement control beside its submit button. Missing or blank terms disable recurring checkout,
and the server refuses a purchase without the terms and explicit agreement. Prices and account codes are attorney-set.
Buying one makes a [Purchase](purchase.md).

An Offering is not a service-catalog service: a service opens a matter through the start door, while an Offering is
invoiced and paid.

- Catalog: [`services-catalog.yaml`](../../neon/locales/en/neon/services-catalog.yaml) Type:
  [`views::locales::services::OfferingCopy`](../../views/src/locales/services.rs)
