---
title: "Project Calendar"
description: >-
  A Project Calendar is the shared Google Calendar on the Firm's Workspace that holds one Project's events, shared with
  its firm-side participants.
---

One secondary Google Calendar per [Project](project.md), owned by the Firm's Workspace service account and named for the
Project code, so no client name reaches the Firm's calendar list. Its Calendar API id is `project.calendar_id`, and the
repository records the same id as `navigator.yaml` `project.calendar_id`.

`navigator project setup` creates the calendar when none is recorded, or adopts an unrecorded one named for the code,
records the id before sharing anything, and shares it as a writer with every firm-side participant — every participation
whose Person is not a client. A re-run shares no one twice and creates no second calendar. A recorded calendar that no
longer exists is reported as `recorded_resource_missing`, never replaced. `navigator project set --calendar <id>`
records an id by hand.

Closing the Project makes every writer a reader and clears the [Booking Link](booking-link.md); the calendar and its
events are kept as part of the matter's record. The matter page links the calendar to the firm side only, as a
[Resource](resource.md).

The calendar is a place for events, not a second store of them. Navigator does not yet write events to it: scheduled
calls and mirrored [Deadlines](deadline.md) are separate work, and the [Nightly Project
Update](nightly-project-update.md) `calendar` tag concerns Deadline rows, not this calendar.

- Provider: [`cloud::calendar`](../../cloud/src/calendar.rs) · setup and close:
  [`store::project_calendar`](../../store/src/project_calendar.rs)
