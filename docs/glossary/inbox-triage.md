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

When DBOS Inbox triage is enabled, a Firm-side Project participant can approve filing one reviewed Message as an
internal [Communication](communication.md). The approved filing names the Message, Project, review revision and current
[Inbox Assignment](inbox-assignment.md). The worker rechecks that scope and the approver's access before it writes.

Changing an email's suggested Project updates its proposal. Each Message requires separate filing approval. Replaying
the same filing reuses the Communication.

The table is `inbox_triage`.

Schema: [`inbox_triage`](../../store/postgres/schema.sql).
