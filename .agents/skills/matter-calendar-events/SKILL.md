---
name: matter-calendar-events
description: >-
  Name every matter calendar after its Project code, and title every event on it with that code, a colon, and then the
  event (for example `acme-auto-ny: INTERNAL – Send the promised review update`). Use when creating, renaming, updating,
  or reviewing any event, deadline, hearing, meeting, tickler, or reminder on a matter calendar, or when creating a new
  matter calendar.
---

# Matter calendar events

Before naming a record or UI concept, follow **Find the term first** in `AGENTS.md`.

A matter calendar and every event on it carry the Project code, so a reader can tell at a glance which matter each event
belongs to when several matter calendars show together.

## Calendar names

- **Name each matter calendar exactly as its Project code**, as `navigator.yaml` and Navigator show it: lowercase, with
  hyphens, such as `acme-auto-ny`.
- **Rename a calendar that doesn't match** to its Project code, then re-prefix its events.
- **Keep matter calendars private** to the firm's lawyers on the matter.

## Event titles

- **Prefix with the Project code.** Every event title begins with the calendar's name (the Project code), then a colon
  and a space, then the rest of the title: `acme-auto-ny: Client call`.
- **Copy the code exactly.** Read it from `navigator.yaml` or the calendar's name; never retype it from memory.
- **Don't repeat the matter.** After the prefix, don't add the client or matter name again.
- **Put a kind marker right after the prefix.** When an event needs a marker, write it in capitals right after the
  prefix, with an en dash before the description. Markers include INTERNAL, TICKLER, VERIFY, VERIFY TIME, and UNVERIFIED
  LIMITATIONS: `acme-auto-ny: VERIFY TIME – Hearing`.
- **Fix as well as create.** When editing an event that lacks the prefix, add it. When asked to revise a matter
  calendar, rename every event that lacks the prefix.
- **Rename quietly.** A rename sends no guest notification. Change the title only; leave the time, guests, reminders,
  description, and visibility as they are.
- **Keep case facts out of the title.** Put the detail and the source in the description.

## Confidentiality

Calendar names, event titles, and descriptions identify clients. Never copy a real Project code, client name, or case
fact into a public surface: a commit, pull request, issue, public repository, website, or log. Use a fictitious code,
such as `acme-auto-ny`, in examples.

## Deadlines

Navigator is the system of record for a matter's dates; the calendar is a view of them. A dated deadline on the calendar
should also exist as a Navigator Deadline. Follow the `deadline` skill to record it.

`navigator project deadline sync` mirrors Deadlines into the Project's shared matter calendar. Don't run it into a
calendar that already holds hand-made events for the same dates, because it duplicates them. After a sync, check that
each mirrored event carries the prefix, and rename any that don't.
