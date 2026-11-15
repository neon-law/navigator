---
title: "Project Calendar"
description: >-
  A Project Calendar is the shared Google Calendar on the Firm's Workspace that holds one Project's events, shared with
  its firm-side participants.
---

One secondary Google Calendar per [Project](project.md), owned by the Firm's delegated Workspace calendar user. Its name
is the Project code, prefixed with `staging-` on staging. Its Calendar API id is `project.calendar_id`, and the
repository records the same id as `navigator.yaml` `project.calendar_id`.

`navigator project setup` creates or adopts the named calendar and records its id before sharing it. Firm-side Lawyer,
Admin, and Owner participants and Clerk participants with a supervising Lawyer DRI receive read access. Setup converts
existing direct writer grants for these participants to reader grants and preserves the calendar owner's access. A
re-run creates no second calendar. A missing recorded calendar is reported as `recorded_resource_missing`. `navigator
project set --calendar <id>` records an id by hand.

Closing the Project makes every writer a reader and clears the [Booking Link](booking-link.md); the calendar and its
events are kept as part of the matter's record. The matter page links the calendar to the firm side only, as a
[Resource](resource.md).

Create, update, complete, and cancel [Deadlines](deadline.md) in Navigator. The mirror reads its own calendar events to
reconcile changes and preserves unrelated events.

On staging, an authorized Firm administrator can inspect a synthetic Project Calendar.

```bash
navigator calendar events --project <code> --start <RFC3339> --end <RFC3339> --host <staging-host>
```

The command returns event IDs, summaries, and start and end times as JSON for a window of up to 31 days. The stored
Calendar ID must match the single calendar the deployment owns with the staging-prefixed Project code as its name.

- Provider: [`cloud::calendar`](../../cloud/src/calendar.rs) · setup and close:
  [`store::project_calendar`](../../store/src/project_calendar.rs)
