---
title: "Email Delivery Attempt"
description: "An Email Delivery Attempt records one admitted send of an Email Receipt to Slack."
---

An Email Delivery Attempt records the correlation key, state, and provider outcome for one admitted send. A repeated
request does not create an attempt unless the delivery state permits a new send.

- Store: [`store::email_deliveries`](../../store/src/email_deliveries.rs)
- PostgreSQL mirror: `email_delivery_attempt`.
