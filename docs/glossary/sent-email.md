---
title: "Sent Email"
description: >-
  The append-only delivery record of one outbound email attempt.
---

The recipient, content, time, and provider outcome recorded by [Email Send](email-send.md). Each row carries SendGrid's
message id, the template that rendered it, and the [Person](person.md) it was addressed to when the sender named one. A
known Project links the delivery to that matter; firm mail may have no Project.

The table is `sent_email`. It lives in PostgreSQL and is append-only: the database refuses any change to a row and any
delete except the retention purge. PostgreSQL keeps one year of rows; the [Sent Email Export](sent-email-export.md)
keeps every day after that.

Schema: [`sent_email`](../../store/postgres/schema.sql).
