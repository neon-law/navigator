---
name: calendar-conflict-check
description: Check Calendar access at Project session start and verify availability before naming or accepting a time.
---

# Check calendar access

Follow **Find the term first** in `AGENTS.md` for the Project Calendar and Attorney Calendar definitions.

At session start, read the Project code and host from `navigator.yaml`. Use `navigator site whoami --host <host>` to
identify the signed-in user. Choose a short upcoming window and run both read-only probes:

```bash
navigator calendar events --host <host> --start <RFC3339> --end <RFC3339>
navigator calendar freebusy --project <code> --attorney <signed-in-email> --host <host> \
  --start <RFC3339> --end <RFC3339>
```

The first command reads the user's primary calendar. The second checks that user's availability alongside the shared
Project Calendar and its Deadlines. Pass the signed-in address every time to keep other users outside the check. Report
any failed read and its reason. The user can register their own calendar with `navigator calendar register --host
<host>`. Refer a share or delegation error to the calendar owner or Workspace admin.

If the session uses the Google Calendar chat connector, also call `list_calendars` with a limit of one. A successful
list confirms connector access only. On a scope error, tell the lawyer which read failed and what the connector
reported. Ask them to reconnect it, start a fresh session so it picks up the grant, and repeat the probe. Keep other
matter work moving while calendar access is being repaired.

Eligible open Project Calendars contribute Busy intervals to Navigator availability through membership. Verify with an
existing Busy event where the participant is absent from the guest list. For a Google booking page, also select the
shared calendar in the appointment schedule's calendars to check for availability.

## Check a proposed time

Before proposing or accepting a time, check the actual window with its UTC offset:

```bash
navigator calendar check --project <code> --attorney <signed-in-email> --host <host> \
  --start <RFC3339> --end <RFC3339>
```

Proceed only when the required reads succeed and the check reports `clear: true`. A conflict or duplicate needs another
window. If a required read fails, report availability as unknown and pause scheduling until it succeeds.
