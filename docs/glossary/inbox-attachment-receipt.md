---
title: "Inbox Attachment Receipt"
description: >-
  Evidence that a Firm received the clean bytes of a bare Asset into its unassigned Inbox.
---

An Inbox Attachment Receipt records that a [Firm](firm.md) received an [Asset](asset.md) through trusted attachment
ingestion. It carries the Firm, Asset, and the server-verified content hash. Identical bytes may have independent
receipts for several Firms without changing the globally deduplicated bare Asset.

The receipt permits linking that bare Asset to an unassigned Message in the receiving Firm's [Inbox](inbox.md).
It grants no Project document access. An attributed Message still requires an Asset belonging to that exact Project.
A supplied Asset identifier, filename, hash, or metadata cannot create a receipt.

The PostgreSQL table is `inbox_attachment_receipt`. Source-authoritative historical attachment migration can establish
receipts from the persisted Message, Conversation, Firm, and verified Asset hash before native Inbox cutover.
