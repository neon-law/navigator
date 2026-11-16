---
name: omnichannel-inbox
description: >-
  Map the omnichannel inbox, triage, inbound email, and relationship-graph surfaces when changing how Navigator
  collects, associates, or displays firm communications. Use when asked to review, file, or archive a matter email.
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

## Review, file, then archive a matter email

1. Open the exact thread in the connected mailbox. Read the messages and check relevant attachments. Identify the matter
   from the record. For court notices, use [case-timeline](../case-timeline/SKILL.md) and distinguish the docket text
   from the service list. Record any identified matter deadline using [deadline](../deadline/SKILL.md).
2. Save the correspondence or a source-linked review in the matter's approved record. Verify the saved record and report
   any material limits, such as redacted identifiers or attachments you could not review.
3. When the user authorizes archiving, archive the exact thread in Gmail after the requested review and filing succeed.
   Use the mailbox's Archive action and verify the thread is out of the inbox. If filing fails or the matter is
   uncertain, leave the thread in the inbox and report what remains. Link the filed record in your report.

## Further reading

- [`docs/email-events-pipeline.md`](../../../docs/email-events-pipeline.md) — outbound event webhook and `sent_emails`.
- [`docs/email-summary.md`](../../../docs/email-summary.md) — the opt-in EmailSummary durable workflow.
- [`docs/inbox-import-cli.md`](../../../docs/inbox-import-cli.md) — `hudson inbox import` semantics.
- [`docs/mail-cli.md`](../../../docs/mail-cli.md) — `hudson mail` Gmail read/draft workflow.

Follow the [Project agent contract](../../../cli/src/projects/agent_contract.md#people-and-entities-in-matter-work) when
matter work identifies a new Person or Entity.
