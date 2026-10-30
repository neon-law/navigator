---
title: "Asset"
description: "An Asset is one row in the assets table that stores a pointer to a static byte artifact."
---

One row in the `assets` table: the canonical store for a static byte artifact. It holds the byte pointer (content type,
byte size, SHA-256, and the storage key from [`cloud::StorageService`](../../cloud/); the bytes live in object storage)
plus, for a matter document, its metadata. Two shapes: a **document asset** (project-scoped, with
`filename`/`kind`/`source`/`received_at`) and a **bare content asset** (a template body or raw `.eml`, those columns
null). Document storage uses Project-scoped content-addressed keys (`projects/<code>/documents/<sha>`); bare content
uses `blobs/<sha>`, an object-storage key that addresses a [Blob](blob.md) and is unrelated to the `blobs/` folder of
[Pointers](pointer.md) in a Project repository. Each write lane dedupes by `sha256_hex`. Merges the former `blobs` +
`documents` split. `visibility` (`internal`, the default, or `client`) gates whether a document asset reaches the
client's view of the matter page and "download all documents" archive; every ingest call site states it explicitly.
Property in an estate plan is an [Estate Asset](estate-asset.md), a different table.

- Schema: [`asset` in `navigator.surql`](../../store/src/schema/navigator.surql) · Write
  lanes: [`store::documents::ingest_bytes`](../../store/src/documents.rs) (document assets),
  [`store::assets::ingest_content`](../../store/src/assets.rs) (bare content).

The table is `asset`.

Schema: [`asset`](../../store/src/schema/navigator.surql#L1622).
