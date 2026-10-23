---
title: "Lead"
description: "A Lead is a public request for contact."
---

A public request for contact. Capture writes a `lead` row: mailbox, optional phone, brand, source path, consent, status,
and submission count. That row is not an identity. The human directory is [Person](person.md). `store::leads::convert`
creates a Client through `store::persons::create` and sets `lead.person_id`. When the mailbox already belongs to a
Person, the queue links that row instead of forking a second one.

Every accepted submission is answered with a signed, 24-hour confirmation link
([`portal::lead_confirm`](../../portal/src/lead_confirm.rs)). Following it converts the lead, marks the address
confirmed, and signs that Client in to their [Purchases](purchase.md). A lawyer-tier Person is never signed in by the
link.

Talking to a lead is attorney work under professional ethics (advertising and solicitation), not a sales sequence.

- Schema: [`lead` in `navigator.surql`](../../store/src/schema/navigator.surql) Queries:
  [`store::leads`](../../store/src/leads.rs)
