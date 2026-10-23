---
title: "Inbox"
description: >-
  An Inbox is the Firm queue of inbound Conversations across supported channels before and after Project assignment.
---

The Firm's queue of inbound Conversations across supported channels. An unassigned Conversation stays on the Firm. A
separate Project attribution operation links it to a Project in the same Firm and records who moved it.

Lawyers open `/app/inbox` from the Inbox card on `/app/lawyer`, also reached through `/app/lawyers`. Channel, Brand, and
unread filters work together. Conversations show their receiving account, sender, interaction type, provider
identifiers, and message timestamps. Firm membership governs the queue; Project participation governs assigned
conversation bodies and replies. Unassigned social replies require the Owner or the Firm's Admin DRI.

- Schema and queries: [`store::inbox`](../../store/src/inbox.rs) —
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)
