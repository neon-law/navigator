---
name: omnichannel-inbox
description: >-
  Review, file, or archive matter email. Map Navigator's inbox, triage, and relationship graph when changing how it
  handles firm communications.
---

# Omnichannel inbox

The inbox collects every supported channel — email, SMS, voice, and the social scrapers — into one firm-wide queue with
project-level and triage views. It is Navigator-side: nothing here syncs into Project repositories, so this skill is
deliberately repo-local and not part of `SYNCED_SKILLS`.

## The three layers

- **`store/src/inbox.rs`** — the PostgreSQL accessors for `inbox_conversation`, `message`, `inbox_ingress`,
  `inbox_assignment`, and `inbox_delivery_attempt`. `store/src/inbox_triage.rs` owns routing. `store/src/conflicts.rs`
  reads the one firm-wide relationship graph (`entity_role`, `relationship`) — it never reads email-derived tables
  directly.
- **`portal/src/inbound_email.rs`** — the SendGrid Inbound Parse webhook: malware scan, raw RFC 5322 archival, lanes
  (summary, lawyer mailbox, letters), then `store::inbox::ingest_inbound`. `portal/src/lawyer_mailbox.rs` keeps a
  lawyer's own outgoing address from entering the queue as a counterparty. `portal/src/email_threads.rs` serves the
  thread API; `portal/src/inbox.rs` the list API.
- **`webapp/src/inbox.rs`** — the Dioxus list, thread, and triage routes under `/app/inbox`, styled through the
  full-width `#inbox` exception in `server/public/css/theme.css`.

## Governing terms

Name these from the glossary before introducing a synonym: Inbox, Inbox Ingress, Inbox Triage, Inbox Assignment,
Message, Conversation, Person, Relationship Edge, Relationship Log. Run `navigator glossary show "<Term>"` for
definitions.

## Invariants

- Inbound email resolves its sender to a Person at ingest — primary email plus `person_mailbox` aliases — and
  persists the association on the conversation. Unknown senders may create a Person row with provenance recorded.
- Automation writes identity and Relationship Log rows only. It never writes a typed `relationship` or `entity_role`
  edge, and never mints a `person_mailbox` alias row.
- An agent never sends, replies to, or forwards email. Draft-only; see [AGENTS.md](../../../AGENTS.md).

## Review, file, then archive

1. Review the exact thread and relevant attachments. Confirm the matter. For court notices, distinguish docket text
   from the service list. Use [case-timeline](../case-timeline/SKILL.md) for filings and
   [deadline](../deadline/SKILL.md) for deadlines.
2. File the email or a source-linked review in the approved matter record. Verify the save and report review limits.
3. With authorization, archive the thread in Gmail and verify it leaves the inbox. If the matter or filing is
   unverified, leave the thread in the inbox. Link the record or explain the blocker.

## Further reading

- [`docs/email-events-pipeline.md`](../../../docs/email-events-pipeline.md) — outbound event webhook and `sent_emails`.
- [`docs/email-summary.md`](../../../docs/email-summary.md) — the opt-in EmailSummary durable workflow.
- [`docs/inbox-import-cli.md`](../../../docs/inbox-import-cli.md) — `hudson inbox import` semantics.
- [`docs/mail-cli.md`](../../../docs/mail-cli.md) — `navigator mail` Gmail read/draft workflow.

Follow the [Project agent contract](../../../cli/src/projects/agent_contract.md#people-and-entities-in-matter-work) when
matter work identifies a new Person or Entity.
