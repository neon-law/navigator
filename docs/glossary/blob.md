---
title: "Blob"
description: "A Blob is the stored bytes of one Document revision, addressed by sha256 in Navigator storage."
---

The stored bytes of one [Document](document.md) revision, addressed by their `sha256` in Navigator storage. Two
revisions with the same bytes are one Blob. A Blob is not a file in Git and carries no metadata: the tracked YAML that
names it, with its asset id, size, `kind`, and `visibility`, is its [Pointer](pointer.md). In a Project checkout a
pulled-down Blob sits in a [Matter Folder](matter-folder.md) under the Git-ignored `documents/`.

The object-storage key prefix `blobs/<sha>` for bare content [Assets](asset.md) addresses a Blob the same way. The
`blobs/` folder of a Project repository holds Pointers, not Blobs.
