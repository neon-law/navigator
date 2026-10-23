---
title: "Soft Deletion"
description: >-
  A Soft Deletion hides a document and its whole revision chain from clients for 180 days before the retention purge
  expunges it.
---

How a [Document](document.md) leaves a Project without being destroyed on the spot. Removing a document's pointer from
the Project repository and running `navigator project sync` asks, one document at a time, whether to soft-delete the
live document. Every [Revision](revision.md) of its [Document Identity](document-identity.md) is stamped together with
`deleted_at`, `purge_after` (`deleted_at` plus 180 days), and the lawyer-tier person who deleted it. Sync records the
commit that removed the pointer. A mistaken chain with no pointer-removal commit can be retired with `navigator site
document retire`; its `deleted_commit_sha` remains empty.

A soft-deleted document drops out of the client's matter page, the downloads a client can reach, and the default
document lists. A lawyer can still read it (`?include_deleted=true`) and restore it with `navigator site document
restore`, which clears the stamps and returns the whole revision history; the next `project sync` writes the pointer
again. A `closed_repository` archive is never soft-deleted.

Once the chain is hidden, the retention lane copies every revision to the private archived-projects bucket, reads each
copy back, and checks its size and SHA-256. Only after every revision verifies does it remove the active copies, and it
keeps any object another unpurged row still needs, such as a sibling chain on the matter sharing the bytes. Until then
the chain keeps its active copies and stays recoverable. A lawyer's read of a revision whose active copy is gone comes
from the verified archived copy, and a restore puts every revision's bytes back at their original keys before it clears
the stamps.

When `purge_after` passes, the retention purge runs the governed expunge on the chain under the `retention` category, so
its audit trail is the one every other expunge leaves, and marks the rows purged. The unattended purge runs daily on
DBOS with the authority each Firm configured, a person who must hold that Firm's recovery-archive capability; a chain
with no such authority is reported and left alone, and no bucket age rule ever stands in for it. Three things stop a
purge without raising an error: a [Legal Hold](legal-hold.md) on the chain, rechecked from fresh rows immediately before
the expunge; a Project that is closed with neither a [Closed Repository](closed-repository.md) archive nor a committed
recovery generation cut after the close; and missing authority. The purge keeps any object another unpurged row needs,
removes the chain's archived copies unless another live or held row needs them, and records each removal so a [Project
Recovery Archive](project-recovery-archive.md) that held them reports a purge rather than damage. A purged document
cannot be restored.

- Write boundary: [`store::assets::soft_delete`](../../store/src/assets.rs)
- Schema: the `deleted_at`, `purge_after`, `deleted_by_person_id`, `deleted_commit_sha`, and `purged_at` fields of
  [`asset` in `navigator.surql`](../../store/src/schema/navigator.surql)
- Purge: [`portal::expunge::purge`](../../portal/src/expunge/purge.rs)
- Archive and restore: [`store::document_retention`](../../store/src/document_retention.rs)
