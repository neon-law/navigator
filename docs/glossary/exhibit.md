---
title: "Exhibit"
description: >-
  An Exhibit is a filed Asset classified with the exhibit kind, a document offered or attached in support of a filing or
  hearing.
---

A document offered or attached in support of a filing, a declaration, or a hearing. In Navigator an exhibit is an
[Asset](asset.md) role, not its own record: a filed byte artifact whose `kind` is `exhibit`. The kind lives on the asset
lane only, so it classifies a filed document and is never a template's declared `kind:`.

Because an exhibit is an asset, it inherits everything an asset carries: its revision chain, its `visibility`, [Soft
Deletion](soft-deletion.md), and [Legal Hold](legal-hold.md). A [Docket Entry](docket-entry.md) links the filed document
it records the same way it links any other asset.

- Vocabulary: [`rules::kind::Kind::Exhibit`](../../rules/src/kind.rs) · Schema:
  [`asset`](../../store/src/schema/navigator.surql)
