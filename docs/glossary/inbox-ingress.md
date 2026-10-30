---
title: "Inbox Ingress"
description: >-
  An internal admission record that prevents duplicate provider messages in a Conversation.
---

Internal plumbing for [Conversation](conversation.md) intake. The Firm, channel, provider account, and provider message
identifier together distinguish one delivery from another.

The table is `inbox_ingress`.

Schema: [`inbox_ingress`](../../store/src/schema/navigator.surql#L2579).
