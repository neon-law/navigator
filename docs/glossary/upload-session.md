---
title: "Upload Session"
description: "An Upload Session is one in-flight streaming upload of a document the buffered upload door cannot take."
---

One row in the `document_upload_session` table: a streaming upload of a [Document Asset](asset.md) larger than
the buffered upload door accepts ([LAW-172](https://linear.app/neon-law/issue/LAW-172)). The row carries the document's
metadata and byte identity exactly as the eventual ingest will file them — `filename`, `kind`, `content_type`,
`visibility`, `description`, `slug`, and the `metadata`/`derived_from`/`signing`/`notarization` extras — plus the byte
identity (`declared_size`, `declared_sha256`), the server-chosen `chunk_size`, and `received_bytes`, the count of
durably staged bytes. The staged chunk bytes live in object storage under `uploads/<session id>/part-<offset>`, named
by [`store::documents::upload_part_key`](../../store/src/documents.rs), never in the database. The client asks for a
session, streams one server-sized chunk per call to the offset `received_bytes` names, and completes; completion
composes the parts, verifies the whole-file SHA-256 against `declared_sha256`, files the bytes through the ordinary
revision ingest, and removes the row and its parts. `received_bytes` is the resume point: a chunk must be exactly
the length that offset names, so a retried append is refused, never applied twice.

A session expires 24 hours after `created_at` and is removed at completion; it is not a [Pointer](pointer.md) (no
revision exists yet) and not a [Blob](blob.md) (no whole bytes are stored until the last chunk lands).

- Schema: [`document_upload_session` in `navigator.surql`](../../store/src/schema/navigator.surql)
- Lanes: [`create_upload_session`, `append_upload_chunk`, `complete_upload`](../../store/src/documents.rs)

The table is `document_upload_session`.
