---
title: "Project Calendar"
description: >-
  A Project Calendar is the shared Google Calendar on the Firm's Workspace that holds one Project's events, shared with
  its firm-side participants.
---

One secondary Google Calendar per [Project](project.md), owned by the Firm's delegated Workspace calendar user. Its name
is the Project code, prefixed with `staging-` on staging. Its Calendar API id is `project.calendar_id`, and the
repository records the same id as `navigator.yaml` `project.calendar_id`.

`navigator project setup` creates or adopts the named calendar and records its id. A new calendar starts with only its
owner. Setup, the Deadline mirror, and closing check the recorded id against the expected name and delegated owner. A
missing or mismatched calendar returns `recorded_resource_missing`. Run `navigator project deadline sync --dry-run` to
check the delegated owner, Project name, and deployment. Preserve the recorded ID while investigating visibility. Use
`navigator project set --calendar <id>` after verifying the intended replacement. A rerun reuses the verified calendar.

When the Firm has a [Workspace calendar owner](firm-calendar-settings.md), DBOS checks assignments on its recorded
calendars every six hours on the [DBOS schedule](../dbos-durability.md#project-calendar-membership). It also provisions
missing calendars for open Projects. A participant needs the matching Firm role, a confirmed address in the owner's
domain, and successful Workspace delegation. Clerks also require a supervising Lawyer DRI. Eligible participants receive
reader access. The job adds the calendar to each participant's Google Calendar list with its events selected, verifies
the entry, and preserves existing display preferences. It requests no sharing email. Navigator includes the calendar in
eligible participants' free/busy checks while the Project is open. Mark an event Busy when it should block scheduling.
Google appointment schedules have their own selection of calendars to check for conflicts.

Create each event once on the shared calendar. Firm participants use calendar membership. Invite clients and other
external guests when they need to attend or respond. Every title begins with the exact Project code and a colon, as in
`sample-litigation: Hearing`. On open Projects, the membership worker corrects editable titles and removes guests on the
owning Firm's Workspace domain or configured [Brand](brand.md) email domains from upcoming events the Project Calendar
organizes. It preserves the organizer, calendar resources, and external guests. It requests no guest notifications.
Google Calendar can accept direct guest edits before the worker reconciles them.

When an assignment ends, the job revokes its reader grant and any event access it marked for that participant. It
refuses automatic sharing on calendars with public or domain-wide access. An hourly [event
backup](project-calendar-event-backup.md) preserves event revisions for the Firm.

Closing the Project makes writers readers and clears the [Booking Link](booking-link.md). The firm-side matter page
links to the shared calendar under Resources until [Project Calendar Retirement](project-calendar-retirement.md)
completes. The event backups remain available after the live calendar is deleted.

Create, update, complete, and cancel [Deadlines](deadline.md) in Navigator. Cancelling a Deadline removes its mirrored
event. The mirror reads its own calendar events to reconcile changes and preserves unrelated events.

Run `navigator project calendar` in the Project checkout or give it `--project <code>`. It lists event IDs, summaries,
and times for the next 24 hours by default. Use `--start` and `--end` for an RFC 3339 window of up to 31 days. The
recorded Calendar ID must match the calendar the deployment owns for the Project.

`navigator project calendar check --start <RFC3339> --end <RFC3339>` checks the Project Calendar, its open
[Deadlines](deadline.md), and the [Attorney Calendars](attorney-calendar.md) of its firm-side participants. The caller
needs firm-side Project access and the Firm's calendar capability.

### Verify a staging calendar

1. Run `navigator project calendar access --project <code> --host <staging-host>`. Read the recorded calendar ID and
   confirm the person is a firm-side participant on the current Project roster. The Project's `readable` field reports
   the deployment's access.
2. Run `navigator project deadline sync --dry-run --project <code> --host <staging-host>`. A successful preview on a
   recorded calendar verifies the delegated owner's access and the expected `staging-<code>` name. Repeat the access
   read and compare the exact ID.
3. In that participant's Google Calendar account, find the ID in their calendar list and read an event on
   it, or confirm a successful empty event read for a bounded window. Record list visibility and event readability
   separately. If events are readable by ID but the calendar is absent from the list, investigate the participant's
   subscription and membership job. Keep the ID and the account details in the private administrative record.

- Provider: [`cloud::calendar`](../../cloud/src/calendar.rs) · setup and close:
  [`store::project_calendar`](../../store/src/project_calendar.rs)
