---
title: "Document Identity"
description: "Document Identity is the (project_id, slug) pair that names a living document in the asset lane."
---

The `(project_id, slug)` pair naming a **living document** in the asset lane — the thing a re-upload updates rather than
duplicates. Deliberately **not unique**: every `assets` row under a slug is one [Revision](revision.md) of that
document, S3-style. A null `slug` means a one-off artifact (an inbound attachment, an executed PDF nobody will revise)
that is a revision of nothing.

The slug is lawyer-chosen and never derived from the filename: a re-upload named `captable_final_v2.pdf` must not fork a
chain, and two unrelated `agreement.pdf` files on one matter must not merge into one. `kind` is immutable across a chain
— a changed kind is a different document, and belongs under its own slug.

Only the asset lane has slug versioning. The notation lane versions already: `templates.is_current` appends a row per
change and every Notation pins a template row.

- Write boundary: [`store::assets::file_revision`](../../store/src/assets.rs) · Schema:
  [`asset` in `navigator.surql`](../../store/src/schema/navigator.surql)

When a new recursive Drive import has no existing source-linked identity, Navigator reserves
`drive/<hex-encoded-provider-file-id>.<initial-extension>` as its slug. The reversible lowercase hexadecimal provider
ID, never the mutable filename or folder, identifies that source. A later import reuses any existing source-linked slug,
including a lawyer's explicit reslug and historical flat-import identities. Renames and moves update placement and
provenance without forking the chain. An extension-changing source rename is refused explicitly because the existing
Pointer format requires the identity's extension to match the source format; it does not create a second identity or
weaken that validation.
