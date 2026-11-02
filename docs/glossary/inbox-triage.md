---
title: "Inbox Triage"
description: >-
  One Message's privilege flag, suggested Project, and the audit trail of how both were decided.
---

The triage row for one inbound [Message](message.md): whether the message is privileged, which [Project](project.md) is
suggested for it, and how the decision was made. Explicitly public social posts are labeled not privileged by rules.
Private social messages and messages whose visibility is unknown stay uncertain until a person reviews them; they never
enter the hosted classifier. Client email and SMS may be flagged privileged by a confident classifier, while other
automated outcomes stay uncertain. A suggested Project is never applied on its own. A person confirms or corrects it,
and the row remains in the review queue until privilege is settled too. The row records these decisions as an auditable
disposition.

The table is `inbox_triage`.

Schema: [`inbox_triage`](../../store/src/schema/navigator.surql#L2628).
