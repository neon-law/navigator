---
title: "Assistant Chat"
description: "An Assistant Chat is one signed-in Person's private exchange with Navigator's assistant."
---

One signed-in [Person](person.md)'s private exchange with Navigator's assistant, a Claude model reached through Vertex
AI. It is owned by that one Person, and nobody else can read it: another caller asking for it gets the same answer as
for a chat that does not exist. Its entries are [Assistant Turns](assistant-turn.md).

An Assistant Chat is not a [Conversation](conversation.md). A Conversation is an exchange with an external account,
belongs to a Firm, and can be attributed to a [Project](project.md); an Assistant Chat has no external party and is
never attributed to a Project.

While it answers, the assistant may call read-only tools from the [Navigator MCP](navigator-mcp.md) catalog, such as the
glossary lookups `navigator glossary list` and `navigator glossary show` offer on the command line. It is never offered
a tool that writes.
