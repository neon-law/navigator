---
title: "Communication"
description: "A Communication is one project-scoped message on a matter, including email, SMS, and an internal note."
---

One message on a matter. The row always names a Project. Email and SMS land here when an Owner assigns the conversation,
and they move with a reassignment. An internal note is a Communication the client does not see and that an external send
does not include.

**A call is a Communication.** A phone call or other off-system contact is logged after the fact on the `call` channel
with its structure beside the note: the call's own direction, the counterparty and their role (`opposing_counsel`,
`client`, `court`, or `other`), when it started and for how long, whether it was recorded and with whose consent, the
issue a meet-and-confer call addressed, and what was agreed. Each agreement stays `unconfirmed` until the confirming
email Communication is logged naming the call. A logged call is internal by default and a call with opposing counsel is
always internal. `navigator project communication create|list` writes and reads it through `GET`/`POST
/app/api/projects/{id}/communications`, at Lawyer tier and scoped to a participant. Communications stay server-only: a
Project repository carries no pointer for one.

An email may record the counterparty's role and an external source message identifier (`--source-ref`). Replaying the
same channel and source identifier returns the existing Communication in that Project. An inbound email may acknowledge
receipt of an outbound email in the same Project through `--acknowledges`. The sent Communication's
`receipt_acknowledged_at` in the conversation list is the earliest linked inbound acknowledgment time, distinct from its
sent time. This records an acknowledgment; it does not independently verify delivery with an email provider.

- Schema and queries: [`store::communications`](../../store/src/communications.rs) —
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)
