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

Navigator mirrors stored [Deadlines](deadline.md) into the Project Calendar. The mirror reads its own events to
reconcile changes and preserves unrelated events.

On staging, an authorized Firm administrator can inspect a synthetic Project Calendar.

```bash
navigator calendar events --project <code> --start <RFC3339> --end <RFC3339> --host <staging-host>
```

The command returns event IDs, summaries, and start and end times as JSON for a window of up to 31 days. The stored
Calendar ID must match the single calendar the deployment owns with the Project code as its name.

- Provider: [`cloud::calendar`](../../cloud/src/calendar.rs) · setup and close:
  [`store::project_calendar`](../../store/src/project_calendar.rs)
