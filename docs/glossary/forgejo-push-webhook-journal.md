---
title: "Forgejo push webhook journal"
description: "The Forgejo push webhook journal archives verified push deliveries before consumption, making delivery recoverable."
---

Every verified Forgejo push-webhook delivery, journaled before its consumption. Hex HMAC-SHA256 verification refuses an
unverified delivery before any write — a refusal is not an event. A Forgejo redelivery re-sends the same body under a new
delivery id, so deduplication is content-based: the unique event index is the (repository, ref updates) pair the store
derives, and a redelivery lands on the same row instead of analyzing twice. The `status` lifecycle is a closed set —
`delivered` and `skipped` are terminal; `failed` is re-consumable and names itself in `failure_detail` — and the schema's
own check constraint is the backstop the store's guard relies on. The change-analysis columns stay NULL until the
analysis step fills them, so a death after analysis never re-reads the Forgejo API. The boot-time replay sweep drains
re-consumable receipts oldest-first, so a death mid-delivery leaves the receipt replayable rather than lost.

- Store: [`store::postgres::forgejo_push_webhook_journal`](../../store/src/postgres/forgejo_push_webhook_journal.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `forgejo_push_webhook_journal`.
