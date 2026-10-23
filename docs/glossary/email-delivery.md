---
title: "Email Delivery"
description: "An Email Delivery tracks Slack dispatch for an Email Receipt."
---

An Email Delivery records dispatch state for one [Email Receipt](email-receipt.md), including provider reconciliation.

Each admitted send records an [Email Delivery Attempt](email-delivery-attempt.md).

- Store: [`store::email_deliveries`](../../store/src/email_deliveries.rs)
- PostgreSQL mirror: `email_delivery`.
