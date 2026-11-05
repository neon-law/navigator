---
title: "E-signature webhook journal"
description: "The e-signature webhook journal archives receipts before consumption, making delivery recoverable."
---

Every verified, parseable e-signature webhook receipt, journaled before its consumption. HMAC verification refuses an
unverified delivery before any write — a refusal is not an event. The receipt's `provider`, `provider_id`, and
`status` triple is the deduplication key: the provider's redelivery of the same event lands on the same row instead of
consuming twice. `consumed_at` stays NULL until consumption succeeds; `attempts` and `last_error` record what a failed
replay did without halting the sweep. The boot-time replay sweep drains unconsumed receipts oldest-first, so a death
mid-consumption leaves the receipt replayable rather than lost.

- Store: [`store::signature_webhook_journal`](../../store/src/signature_webhook_journal.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `esignature_webhook_journal`.
