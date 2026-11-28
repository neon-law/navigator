---
name: deadline
description: >
  Record a matter's Deadline once in Navigator, then surface it on the matter page and in the Project's private Slack
  channel with reminders ahead of the date. Trigger whenever a deadline, hearing, filing cutoff, response date,
  registration cutoff, or reminder is requested or discovered in a document, order, email, or conversation. Synced byte
  for byte into every Project repository's `.agents/skills/` from this canonical copy.
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
calendar role, and due date before writing. Use its ID when amending an existing date.

## 2. Verify the date and its authority

- **Where it binds.** Use `statute`, `court_rule`, `case_law`, `court_order`, `contract`, or `internal`.
  `statute` and `court_rule` require a citation in `--authority`; verify it as the `case-downloader` skill does.
- **Whose date it is.** `--calendar-role` is `firm` (our obligation), `opponent` (the other side's), or `hearing` (an
  appearance). A hearing is not a filing obligation, and its date is owned by the docket: record the appearance as a
  Docket Entry through the `case-timeline` skill (`navigator project docket add --kind hearing --scheduled-on …`), and
  use a `hearing` row here only for the reminder about it.
- **What kind of date it is.** `--category` is `court` (a filing or response the court requires), `hearing` (a scheduled
  appearance), `discovery` (a discovery request, response, disclosure, or cutoff), `internal` (a firm preparation
  target), or `tickler` (a follow-up reminder). A `hearing` role defaults to the `hearing` category and an `internal`
  authority kind to `internal`; otherwise name the category. Court and discovery dates can look alike by authority, and
  the category sets the row's fixed color on every mirrored Google Calendar.
- **Court date versus preparation target.** A court deadline and the internal date by which a draft must be ready are
  two rows: the court date under its rule or order, the preparation target as `internal`.
- **Date or instant.** Use `--due-on YYYY-MM-DD` for an all-day Deadline in the matter’s time zone. Supply
  `--due-timezone Area/City` when an event has its own governing zone. Include `--cutoff HH:MM:SS` only when the source
  names a local cutoff. Use `--due-at` with an explicit RFC 3339 offset for an exact instant. A hearing requires a timed
  start. Use `--hearing-minutes` for the duration in the notice and `--hearing-location` for its address. Without a
  duration, the calendar reserves an hour.
- **Never compute court days.** Navigator stores the date; it never derives one. If the date depends on counting court
  days, service-method extensions, or holidays, the lawyer DRI confirms the count and the authority records the rule.
- **Never assume relief.** Do not record an extension, a continuance, or a stipulated date until the order or the signed
  stipulation exists. File that document first and pass it as `--source-document`.

If a fact is missing or unverified, stop and ask the lawyer DRI. Do not record a guess.

## 3. Record it

Queue a court date with `navigator project deadline review queue --file <private-file>`. For an express date include the
highlighted signed order. For a calculated date include the highlighted trigger and archived governing Authority. The
lawyer inspects the actual source pages in **Court date review** and confirms the individual revision. Include the
existing Deadline ID on an amendment. DBOS releases the confirmed date to calendar sync and ICS. A clerk may prepare or
correct the proposal.

Use direct creation for an internal or contractual obligation.

```bash
navigator project deadline create \
  --title "Review draft agreement" \
  --due-at 2026-11-02T17:00:00-08:00 \
  --authority-kind contract \
  --authority "Synthetic agreement, section 4" \
  --calendar-role firm \
  --category internal \
  --source lawyer \
  --source-document <document id> \
  --owner <lawyer DRI person id> \
  --internal-lead-days 7 \
  --client-lead-days 3 \
  --visibility client
```

- **Name what is due.** Creation requires `--title`, a short name for the pleading or response (at most 200 characters).
  Rows recorded without one show only the authority and role. `update <id> --title ""` clears it.
- **Name the category.** `update <id> --category ""` clears it; an update without the flag keeps it.
  Review any `category_review_required: true` row and choose its category with `update <id> --category <category>`.
  Never infer `court` versus `discovery` from the authority kind alone.
- **Explain the obligation.** `--description` adds optional detail; an empty one on update clears it.
- **Choose visibility.** Creation defaults to `--visibility internal`. Selecting `internal` clears the client lead;
  `--visibility client` requires `--client-lead-days` and the store's internal lead. Updates preserve the current choice
  unless visibility or the lead is changed explicitly.
- **Lead times are visibility.** `--internal-lead-days` warns the firm. `--client-lead-days` is the only way a client
  ever sees a deadline: with it unset, the date stays internal on the matter page and in every reminder. The internal
  lead is never shorter than the client lead.
- **Internal-only instructions win.** If the lawyer says a date is internal, leave `--client-lead-days` unset.
- Read back the printed row: confirm the due instant, role, category, authority, and lead times before going further.

Change a recorded date, or close one when it is met or no longer applies. Neither close deletes the row.

```bash
navigator project deadline update <id> --due-at 2026-11-09T17:00:00-08:00
navigator project deadline complete <id>
navigator project deadline cancel <id>
navigator project deadline reopen <id>
```

Put the docket on a calendar with `navigator project deadline ics`. It prints every row as one RFC 5545 `VCALENDAR`, one
`VEVENT` per Deadline. The `UID` is `<deadline id>@<host>` and `SEQUENCE` is the row's monotonic calendar revision, so
re-importing the file after an `update` changes the same calendar event. A cancelled row renders `STATUS:CANCELLED`, and
`--internal-lead-days` becomes the reminder alarm. The summary starts with the Project code, followed by a colon, its
category when known, and the action. Civil dates render as all-day events. Court Deadlines also carry reminders at
seven, three, and one day.

For a single saved event, pass `--ics <private path>` to `create` or `update`. The command prints the stored row and
writes that row as a calendar file. `--location` requires `--ics` and copies a court-supplied location into that file
only. Keep the file outside Git: a registration link may be personal. If export fails after the write, the error names
the saved Deadline; update that row or export the feed instead of creating a replacement.

Reopen only when a cancelled date applies again. It restores the same row (id, replay key, and fields) to open.
Reopening an open row is a no-op; a complete row cannot be reopened. Replaying a cancelled row refuses until you reopen
it; then update or replay the same key. Never create a replacement row. Replaying a complete row updates its fields and
keeps it complete.

## 4. Check the matter page

Open `/app/projects/<code>` and confirm the Deadlines panel lists the row with the right date, role, and overdue state.
A client sees only rows that carry a client lead time.

## 5. Verify Slack reminders

When the Firm has connected Slack, the worker sends reminders to the Project's recorded private channel. It checks the
current open, confirmed Deadline before each post. The reminder tags the Lawyer DRIs and the firm-side deadline owner
whose Slack accounts resolve. It includes what is due, the governing date or cutoff, the authority, the matter page, and
the Deadline ID. For timed deadlines, Slack also displays the cutoff in the reader's local time.

Reminders go out from 8 a.m. in the governing time zone on the internal lead date, the previous weekday, and the due
date. Earlier cutoffs move the due-day reminder earlier. Overlapping reminders produce one post. An open Deadline gets
one overdue escalation within 24 hours of the cutoff. Completing or cancelling the Deadline stops future reminders.

When completing or cancelling a Deadline, post a status note with its ID if Slack is connected. Use the
`private_slack_channel` listed in `navigator.yaml` for that manual note.

Use the channel's actual message when verifying delivery. Keep client identities and matter facts out of any manual
follow-up beyond what the private channel already holds.

## 6. Calendar mirror (optional)

Mirror a Deadline to Google Calendar only when the lawyer asks and names the calendar. Title the event as the
`matter-calendar-events` skill says: the Project code, a colon, then the action. Put the matter-page link in the
description, and leave client names and case facts out of the title.

## 7. Court appearance registration is separate

Recording a `hearing` row does not register anyone to appear. Registration is its own verified action: complete it on
the court's system, file the confirmation as a Document, and report any pending eligibility, admission, or hearing issue
to the lawyer DRI.

## 8. Report what succeeded

Report the Deadline ID and read-back, the matter page, any verified Slack delivery, and any calendar event. Report
future Slack reminders as pending while they await their delivery time. Report any surface that failed or was
unavailable.
