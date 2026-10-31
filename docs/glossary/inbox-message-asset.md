---
title: "Inbox Message Asset"
description: >-
  One attachment on an inbound Inbox Message, stored as a content-addressed Asset and ordered as the provider sent it.
---

An [Inbox Message](inbox.md) may carry several attachments. Each one is ingested through the bare-blob lane of
[Asset](asset.md) — content-addressed by sha256, so the same bytes reuse the same Asset — and recorded on a link row
that keeps the original filename and the attachment's position in the provider's order. The first link is the Message's
primary attachment, the one a Communication projection references. Re-ingesting the same attachment bytes on a message
reuses the link, so a retried delivery cannot duplicate one.

The table is `inbox_message_asset`. Schema: [`inbox_message_asset`](../../store/src/schema/navigator.surql).
