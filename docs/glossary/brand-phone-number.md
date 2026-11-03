---
title: Brand Phone Number
description: "A Firm's registered Twilio receiving number: the E.164 callee that maps an inbound webhook to its Brand."
---

A **Brand Phone Number** is one of the [Firm](firm.md)'s receiving numbers, registered in PostgreSQL: an E.164 callee,
the [Brand](brand.md) it speaks for, the Twilio account it lives in, a reference to that number's own KMS-backed
credential, and the lanes it carries (`voice`, `sms`, or `both`). The `number_e164` is the unique key: one number
belongs to at most one Brand, so a webhook's callee (`To` for messages, `From` for delivery callbacks) resolves to
exactly one tenant — [Brand](brand.md), then [Firm](firm.md) — before anything is ingested.

The registry replaces the one-deployment, one-number configuration. Adding a Brand number is data, not a recompile:
`store::brand_phone_numbers::put` is idempotent for a repeated identical registration and conflicts loudly when the same
number is re-registered with different content, so a token rotation can never silently rewrite history.

Every Twilio webhook lane resolves through the registry spine in `portal::twilio_resolution`: look the callee up, fail
closed on an unknown number, verify Twilio's signature against that number's own credential at constant time, and only
then ingest. A registration whose credential cannot be unsealed refuses the lane; an unregistered callee is a 404 that
ingests nothing.

- Schema: `brand_phone_number` in [`store/postgres/migrations/`](../../store/postgres/) ·
  [`store::brand_phone_numbers`](../../store/src/brand_phone_numbers.rs) ·
  [`portal::twilio_resolution`](../../portal/src/twilio_resolution.rs)
