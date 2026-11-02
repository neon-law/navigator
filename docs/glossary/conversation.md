---
title: "Conversation"
description: "A Conversation is an exchange with an external account, scoped to a Firm and supported channel."
---

An exchange on a supported channel with an external account. It belongs to a Firm from the moment it arrives. A Project
sees it only after a separate attribution operation, and a reassignment moves the history to the new Project.

- Schema and queries: [`store::inbox`](../../store/src/inbox.rs) —
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)

The table is `inbox_conversation`.

Schema: [`inbox_conversation`](../../store/postgres/schema.sql).
