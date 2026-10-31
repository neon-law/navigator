---
title: "Inbox Process"
description: >-
  The reviewable pass that classifies a lawyer's unprocessed inbox message onto a route and records the verdict.
---

The closed pass over the Firm's unprocessed lawyer-mailbox [Messages](email-conversation-message.md): a run classifies
each message server-side onto a route, shows the verdict as a plan first, and records it only on an explicit apply. The
route — `nef`, `claims_agent`, `chambers`, or `other` — is computed from transport facts (sender address, subject, and
body markers), never posted by a client, so the plan a lawyer reviews and the verdict that is applied are the same
computation. It is advisory: it names the desk a message belongs to, and it files nothing by itself.

`other` is the explicit fallback, not a silence: applying it still records the route and the processing time, so the
backlog stays honest about what was reviewed. The route and processing time ride on the message row as `route` and
`processed_at`, and a re-run overwrites them with the same truth. A Slack notice to the assigned matter's channel is
best-effort and metadata-only. Archiving the Gmail copy — removing the `INBOX` label, never trashing — is a separate CLI
action taken after an apply.

- Route vocabulary: [`store::inbox::route`](../../store/src/inbox.rs)
- Classifier: [`portal::inbound_email`](../../portal/src/inbound_email.rs)
- CLI verb: `navigator inbox process` ([`cli::inbox`](../../cli/src/inbox.rs))
