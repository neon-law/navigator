---
title: "Message"
description: "A Message is one inbound, outbound, or internal entry in a Conversation transcript."
---

One inbound, outbound, or internal entry in a [`Conversation`](conversation.md). The shared transcript records delivery
state, authorship, attachments, provider identity, and any project-scoped [`Communication`](communication.md)
projection. Social messages remain on the Brand's Conversation until a separate assignment operation attributes them to
a Project.

New inbound Messages retain their Slack notice state and receipt on the original record. A pending notice is recovered
by the scheduled reconciler. The worker reserves it before sending, stores the Slack channel and timestamp after
confirmation, and keeps an ambiguous send unknown until a correlated history receipt confirms it. An unknown notice is
never blindly sent again. The deployment's messages channel belongs to `SLACK_MESSAGES_FIRM_ID`, or the Firm wearing
Neon when that setting is absent. Other Firms and missing configuration produce a disabled notice without exposing
message bodies. Historical Messages without a notice state are not replayed.

- Schema and queries: [`store::inbox`](../../store/src/inbox.rs) —
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)

The table is `message`.

Schema: [`message`](../../store/src/schema/navigator.surql#L2551).
