---
name: deadline
description: >
  Record a matter's Deadline once in Navigator, then surface it on the matter page and in the Project's private Slack
  channel with reminders ahead of the date. Trigger whenever a deadline, hearing, filing cutoff, response date,
  registration cutoff, or reminder is requested or discovered in a document, order, email, or conversation. Kept
  identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy lives in Navigator's
  own `.agents/skills/`.
---

# Deadline

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`, then read
the term itself:

```bash
navigator glossary show "Deadline"
```

A Deadline is the firm's docket. A missed one is a malpractice event, so it is recorded once, in Navigator, and every
other surface (the matter page, Slack, a calendar) is a view of that record. A saved schedule document, a calendar
event, or a Slack reminder is never a Deadline on its own.

## 1. Read what is already recorded

```bash
navigator project deadline list
```

The command reads the Project code from `navigator.yaml`. Match the new date against existing rows by authority,
calendar role, and due date before writing. If a row already covers it, update that row; never write a second one.

## 2. Verify the date and its authority

- **Where it binds.** Pick the `authority_kind`: `statute`, `court_rule`, `court_order`, `contract`, or `internal`.
  `statute` and `court_rule` require a citation in `--authority`; verify it as the `case-downloader` skill does.
- **Whose date it is.** `--calendar-role` is `firm` (our obligation), `opponent` (the other side's), or `hearing` (an
  appearance). A hearing is not a filing obligation, and its date is owned by the docket: record the appearance as a
  Docket Entry through the `case-timeline` skill (`navigator project docket add --kind hearing --scheduled-on …`), and
  use a `hearing` row here only for the reminder about it.
- **Court date versus preparation target.** A court deadline and the internal date by which a draft must be ready are
  two rows: the court date under its rule or order, the preparation target as `internal`.
- **The instant.** `--due-at` is RFC 3339 with an explicit offset, in the court's or counterparty's time zone (for
  example `2026-11-02T17:00:00-08:00`). A bare date is refused.
- **Never compute court days.** Navigator stores the date; it never derives one. If the date depends on counting court
  days, service-method extensions, or holidays, the lawyer DRI confirms the count and the authority records the rule.
- **Never assume relief.** Do not record an extension, a continuance, or a stipulated date until the order or the signed
  stipulation exists. File that document first and pass it as `--source-document`.

If a fact is missing or unverified, stop and ask the lawyer DRI. Do not record a guess.

## 3. Record it

```bash
navigator project deadline create \
  --due-at 2026-11-02T17:00:00-08:00 \
  --authority-kind court_order \
  --authority "Order setting briefing schedule, entered 2026-10-05" \
  --calendar-role firm \
  --source lawyer \
  --source-document <document id> \
  --owner <lawyer DRI person id> \
  --internal-lead-days 7 \
  --client-lead-days 3
```

- **Lead times are visibility.** `--internal-lead-days` warns the firm. `--client-lead-days` is the only way a client
  ever sees a deadline: with it unset, the date stays internal on the matter page and in every reminder. The internal
  lead is never shorter than the client lead.
- **Internal-only instructions win.** If the lawyer says a date is internal, leave `--client-lead-days` unset.
- The command prints the stored row. Read it back and confirm the due instant, role, authority, and lead times match
  what you meant before going further.

Change a recorded date, or close one when it is met or no longer applies. Neither close deletes the row.

```bash
navigator project deadline update <id> --due-at 2026-11-09T17:00:00-08:00
navigator project deadline complete <id>
navigator project deadline cancel <id>
navigator project deadline reopen <id>
```

Reopen only when a cancelled date applies again. It restores the same row to open, retaining its id, replay key, and
stored fields. Reopening an open row is a no-op; a complete row cannot be reopened. Replaying a cancelled row refuses
without changing it and directs you to reopen first. Reopen it, then update or replay the same key; never create a
replacement row. Replaying a complete row updates its fields while keeping its complete status.

## 4. Check the matter page

Open `/app/projects/<code>` and confirm the Deadlines panel lists the row with the right date, role, and overdue state.
A client sees only rows that carry a client lead time.

## 5. Remind the channel

Post reminders in the Project's **private** Slack channel, named by `private_slack_channel` in `navigator.yaml`. Never
post a deadline in a channel shared with the client, and never in a channel you have not confirmed is that one.

Each reminder carries:

- the action required, in one line;
- the due instant in the court's time zone and in the lawyer DRI's local time;
- the authority (`court_order`, rule, or citation);
- a mention of the owner, when one is recorded;
- the link to `/app/projects/<code>`;
- the Deadline id, so a later run can find the reminders that belong to it.

Schedule them with the Slack connector's scheduled message: one at the internal lead, one the business day before, and
one on the morning of the due date. When a Deadline is completed or cancelled, post a short note in the same channel
saying so and naming the id. The connector cannot delete a scheduled message, so tell the lawyer DRI which scheduled
reminders to remove.

Escalate an open Deadline that is past due: post again, mention the owner and the lawyer DRI, and say it is overdue.

Keep client identities and matter facts out of every reminder beyond what the private channel already holds.

## 6. Calendar mirror (optional)

Mirror a Deadline to Google Calendar only when the lawyer asks and names the calendar. Use a neutral title (the action
and the Project code), put the matter-page link in the description, and leave out client names and case facts. Never
invent a court video link; copy one only from the court's own notice.

## 7. Court appearance registration is separate

Recording a `hearing` row does not register anyone to appear. Registration is its own verified action: complete it on
the court's system, file the confirmation as a Document, and report any pending eligibility, admission, or hearing issue
to the lawyer DRI.

## 8. Report what succeeded

End with what actually happened on each surface: the Deadline row (id and read-back), the matter page, each Slack
message or scheduled reminder, and any calendar event. If a surface failed or is unsupported, say so plainly. Do not
report a reminder as set when it was not.
