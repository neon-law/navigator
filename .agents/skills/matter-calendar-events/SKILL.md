---
name: matter-calendar-events
description: >-
  Name every matter calendar after its Project code, and title every event on it with that code, a colon, and then the
  event (for example `acme-auto-ny: INTERNAL – Send the promised review update`). Use when creating, renaming, updating,
  or reviewing any event, deadline, hearing, meeting, tickler, or reminder on a matter calendar, or when creating a new
  matter calendar. When the matter's Firm has no shared Project Calendar, hand out ICS files in ~/Downloads instead.
---

# Matter calendar events

Before naming a record or UI concept, follow **Find the term first** in `AGENTS.md`.

A matter calendar and every event on it carry the Project code, so a reader can tell at a glance which matter each event
belongs to when several matter calendars show together.

## Calendar names

- **Name each matter calendar exactly as its Project code**, as `navigator.yaml` and Navigator show it: lowercase, with
  hyphens, such as `acme-auto-ny`.
- **Rename a calendar that doesn't match** to its Project code, then re-prefix its events.
- **Keep matter calendars private** to eligible firm-side participants on the matter.

## Sharing and attendance

Create each event once on the shared calendar. Membership gives eligible participants calendar access and includes Busy
events in their Navigator availability. Invite a person to an individual event when attendance or an RSVP is needed.
Before removing an existing invitation, verify calendar access, the scheduling result, and the organizer's intent.

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

## When the Firm has no Project Calendar

Only a Firm with Google Calendar switched on in its Firm Integrations has a shared Project Calendar. G&S has one; other
Firms do not. Without one, don't create a Google calendar for the matter. Record every date as a Navigator Deadline and
give the lawyer an ICS file to open instead.

1. **Record the Deadline and write the file in one step.** Use the court's or the meeting's own time zone, and save the
   file in `~/Downloads`, named after the Project code and the event:

   ```bash
   navigator project deadline create --project acme-auto-ny \
     --title "Status conference" --calendar-role hearing --authority-kind order \
     --due-at 2026-12-08T11:00:00-05:00 --due-timezone America/New_York --hearing-minutes 60 \
     --location "Zoom https://example.zoomgov.com/j/0000000000 · Meeting ID 000 0000 0000 · Passcode 000000" \
     --ics ~/Downloads/acme-auto-ny-status-conference.ics
   ```

2. **Carry every joining detail the court sent.** Put the video link, meeting ID, passcode, dial-in number, and the
   courtroom or address in `--location`, copied exactly from the notice. Put the source in `--description`.
3. **Check the file before handing it over.** Open it and confirm the `DTSTART` carries the right `TZID` (or a UTC time
   that converts to the court's local time) and that the location kept the full link.
4. **Tell the lawyer where it is.** Name the file in `~/Downloads`; they open it to add the event to their own calendar.
   An ICS file can hold a personal registration link, so never commit it or attach it anywhere else.

To change the date later, run `navigator project deadline update <id> --ics <same path>` and hand out the new file. It
keeps the same `UID`, so importing it updates the event instead of adding a second one.
