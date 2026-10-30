---
title: "Inbox Triage"
description: >-
  One Message's privilege flag, suggested Project, and the audit trail of how both were decided.
---

The triage row for one inbound [Message](message.md): whether the message is privileged, which [Project](project.md) is
suggested for it, and how the decision was made. The privilege label is structural, not statistical: a message that
arrives on a social channel is never privileged, decided in code from the channel alone, and no classifier output can
change it. Client email may be privileged, and automation may only ever mark it privileged or uncertain; `uncertain`
sends the message to a person. A suggested Project is never applied on its own — a person confirms it, and the row
records the decision as an auditable disposition.

The table is `inbox_triage`.

Schema: [`inbox_triage`](../../store/src/schema/navigator.surql#L2628).
