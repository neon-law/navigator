---
title: "Assistant Turn"
description: "An Assistant Turn is one ordered entry in an Assistant Chat."
---

One ordered entry in an [Assistant Chat](assistant-chat.md). A turn is exactly one of four kinds:

- **prompt** — what the Person typed.
- **reply** — the assistant's answer.
- **tool call** — a read-only [Navigator MCP](navigator-mcp.md) tool the assistant chose, with its arguments.
- **tool result** — what that tool returned, paired to its call.

A turn records the model's token counts so usage can be measured per Person. An Assistant Turn is not a
[Message](message.md): a Message is an entry in a [Conversation](conversation.md) with an external account, carrying
delivery state and provider identity that an Assistant Turn never has.

The table is `assistant_turn`.

Schema: [`assistant_turn`](../../store/src/schema/navigator.surql).
