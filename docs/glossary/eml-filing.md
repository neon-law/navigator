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
- CLI: `navigator mail file --eml <path>` —
  [`cli/src/mail.rs`](../../cli/src/mail.rs)

The message's `Message-ID` header scopes the source reference (`eml:<message-id>`, else a content digest), the same
reference shape the stored-inbound `mail/file` door uses for provider message ids. An optional firm mailbox marks the
message outbound when it matches the `From` header; without it the message files as inbound. Filing is Lawyer-tier,
matter-scoped, and draft-only in spirit: nothing is ever sent.

The table is `communication` — .eml filing adds no new table.
