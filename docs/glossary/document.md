---
title: "Document"
description: "A Document is a Project-scoped Asset with searchable metadata and a content-addressed byte pointer."
---

A matter document — a project-scoped [Asset](asset.md) carrying the metadata callers see (`filename`, `kind`, `source`,
`received_at`) alongside the byte pointer. `kind` is a closed asset-lane classification from
[`rules::kind::Kind`](../../rules/src/kind.rs) (`valid_for(Lane::Asset)`). `POST /app/api/projects/{id}/documents` and
`navigator site document upload` require it; omitted or blank is `400 kind_required`. Inbound email attachments still
file as `unclassified`.

An OCR transcript is another `transcript` Document revision. Its `derived_from` field records the source Document's
asset id, revision number, and SHA-256, so a live document check can tell when the source has moved on. Its
`transcript_quality` is `machine` until a lawyer proofreads and uploads a newer revision as `proofread` on the same
slug. OCR text belongs in object storage and an ignored local body, never in Project Git. `navigator site document
transcribe <pointer>` renders and OCRs each PDF page locally.

> **Source of truth = object storage plus the assets row.** When the application generates or proxies a document (a
> rendered retainer PDF, a raw inbound email body), the bytes land in object storage via
> [`cloud::StorageService`](../../cloud/) and the `assets` row is the canonical pointer. A Project repository carries
> the source-side document contract in two root folders: `blobs/` holds each document's committed [Blob](blob.md), and
> `documents/` holds the Git-ignored bytes staged for, or pulled down by, `navigator project sync`. Sync uploads each
> staged file, writes its Blob, and keeps the local copy. Neither folder is part of the portal bundle or a second
> document store. Raw legal-document bytes never enter Project Git.

- Schema: [`asset` in `navigator.surql`](../../store/src/schema/navigator.surql) Queries:
  [`store::assets`](../../store/src/assets.rs) Lives in: the `asset` table in SurrealDB (document-shaped rows)
