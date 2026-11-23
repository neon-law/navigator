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
missing or mismatched calendar returns `recorded_resource_missing`. Verify the intended calendar and use `navigator
project set --calendar <id>` to correct the link. A rerun reuses the verified calendar.

The opted-in DBOS membership job checks recorded calendars after setup and rechecks assignments in each five-minute
slot. A Project participant needs the matching Firm role, a confirmed address in the owner's Google Workspace domain,
and successful Workspace delegation. Clerks also require a supervising Lawyer DRI. Eligible participants receive reader
access. Eligible Clerks, Lawyers, and Admins are invited to events organized by the Project Calendar, including
recurring series. Google controls invitations on events organized elsewhere. The job preserves existing manual grants
and guests and removes the direct access and event guests it added when an assignment ends. It refuses automatic sharing
on calendars with public or domain-wide access.

Closing the Project makes every writer a reader and clears the [Booking Link](booking-link.md); the calendar and its
events are kept as part of the matter's record. The matter page links the calendar to the firm side only, as a
[Resource](resource.md).

Create, update, complete, and cancel [Deadlines](deadline.md) in Navigator. The mirror reads its own calendar events to
reconcile changes and preserves unrelated events.

Run `navigator project calendar` in the Project checkout or give it `--project <code>`. It lists event IDs, summaries,
and times for the next 24 hours by default. Use `--start` and `--end` for an RFC 3339 window of up to 31 days. The
recorded Calendar ID must match the calendar the deployment owns for the Project.

`navigator project calendar check --start <RFC3339> --end <RFC3339>` checks the Project Calendar, its open
[Deadlines](deadline.md), and the [Attorney Calendars](attorney-calendar.md) of its firm-side participants. The caller
needs firm-side Project access and the Firm's calendar capability.

- Provider: [`cloud::calendar`](../../cloud/src/calendar.rs) · setup and close:
  [`store::project_calendar`](../../store/src/project_calendar.rs)
