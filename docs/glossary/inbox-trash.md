---
title: "Inbox Trash"
description: >-
  The Inbox Trash holds Conversations a lawyer moved out of the Inbox; nothing in it is deleted, and new mail restores a
  Conversation.
---

Where a [Conversation](conversation.md) goes when an Owner, Lawyer, or Clerk of its Firm moves it out of the
[Inbox](inbox.md). Trash is a view, not a deletion: the Conversation, its Messages, its attachments, and its Project
assignment all stay as they are, and no purge ever reads the stamp. It is not a [Soft Deletion](soft-deletion.md).

A trashed Conversation leaves the Inbox and Sent folders and a Project's unread count, and appears in the Trash folder
(`?trash=true`). The same control restores it. A new inbound Message on the Conversation restores it too, so mail from a
trashed thread is never hidden. Billing and reporting keep reading every Conversation, trashed or not.

- Write boundary: [`store::inbox::set_trashed`](../../store/src/inbox.rs)
- Schema: the `trashed_at` field of [`inbox_conversation`](../../store/postgres/schema.sql)
