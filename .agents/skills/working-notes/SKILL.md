---
name: working-notes
description: >
  Preserve work in flight across sessions in a private, git-ignored handoff note. Trigger before ending a session with
  unfinished work, near compaction, or when the lawyer requests a handoff. Synced verbatim into every Project
  repository's `.agents/skills/`; this canonical copy lives in Navigator's own `.agents/skills/`.
---

# Working notes

## Find the term first

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

Keep the current plan in `notes/HANDOFF-<topic>.md.local`. This git-ignored note stays inside the Project repository;
never commit it or paste its matter facts into an outside tracker. The gate does not read it.

Write it before a session ends with work in flight, near compaction, or when the lawyer asks. Rewrite it when the plan
changes; never append a contradiction. Read it at the next session's start and check pending actions against Navigator.

Use these sections in order, with one fact per line and absolute dates with a time zone. Cite message ids, replay keys,
and PR numbers without retelling their contents.

## Plan

State the next actions and the rule behind each computed date.

## Files

List paths, the render command, and open placeholders.

## Drafts waiting

List unsent drafts and when each goes to the lawyer for review and personal sending.

## Deadlines

Identify the Navigator rows that hold the dates.

## Reminders

Identify scheduled messages and flag stale ones. A note does not create, cancel, or reschedule a reminder.

## Open items

List unresolved questions and dependencies.

When the work lands, log what belongs in the record as a Navigator Communication, then delete the note.
