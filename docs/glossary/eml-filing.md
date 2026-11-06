---
title: ".eml Filing"
description: ".eml filing records a raw RFC 822 message as a matter Communication, server-side, with each
attachment filed as a linked document."
---

Filing a raw RFC 822 message (an `.eml` file or export) as a matter Communication without any per-machine Gmail
authorization. The portal parses the MIME server-side, files the message through the idempotent `(channel, source_ref)`
Communication ingest, and files each attachment as a linked document under a deterministic slug (`mail/<message
reference>/<index>`). Re-uploading the same message dedupes rather than writing again.

- Door: `POST /app/api/projects/{id}/mail/eml` —
  [`portal/src/api.rs`](../../portal/src/api.rs), parser in [`portal/src/eml_filing.rs`](../../portal/src/eml_filing.rs)
- CLI: `navigator mail parse <path>.eml` —
  [`cli/src/mail.rs`](../../cli/src/mail.rs)

The message's `Message-ID` header scopes the source reference (`eml:<message-id>`, else a content digest), the same
reference shape the stored-inbound `mail/file` door uses for provider message ids. The firm mailbox — the request's
`mailbox`, else the signed-in lawyer's own address — marks the message outbound when it matches the `From` header;
otherwise the message files as inbound. Attachments require a `kind`, and a refusal names its reason as a structured
`error` code and `message`. Filing is Lawyer-tier, matter-scoped, and draft-only in spirit: nothing is ever sent.

No new table is introduced: .eml filing files through the existing `communication` table, so nothing new appears in the
schema or the Postgres mirror.
