---
title: "Legal Hold"
description: >-
  A Legal Hold keeps a document's whole revision chain out of the retention purge, however long it has been soft-
  deleted.
---

A stamp (`legal_hold_at`) a lawyer places on a [Document Identity](document-identity.md), set on every
[Revision](revision.md) of the chain together. It does not hide or lock the document: its only effect is that the
retention purge skips a held chain, so a [Soft Deletion](soft-deletion.md) past its 180 days is reported as held rather
than expunged. The purge rechecks the hold from fresh rows immediately before it expunges, so a hold placed while a run
is pending still protects the chain, and it protects the chain's archived copies as well as its active ones. Releasing
the hold lets the next purge run take it.

- Write boundary: [`store::assets::set_legal_hold`](../../store/src/assets.rs) via
  `POST /app/api/projects/{id}/documents/{asset_id}/hold` and `navigator site document hold`
