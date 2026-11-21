---
name: calendar-conflict-check
description: Check Calendar access at Project session start and verify availability before naming or accepting a time.
---

# Check calendar access

At the start of a Project session, read the Project code and host from `navigator.yaml`. Run the read-only probe once:

```bash
navigator calendar access --project <code> --host <host>
```

Report any unrecorded or unreadable calendars and the reported reason. An attorney can record their own calendar with
`navigator calendar register --host <host>`. A share or delegation error needs the calendar owner or Workspace admin.

If the session uses the Google Calendar chat connector, also call `list_calendars` with a limit of one. A successful
list confirms connector access only. On a scope error, tell the lawyer which read failed and what the connector
reported. Ask them to reconnect it, start a fresh session so it picks up the grant, and repeat the probe. Keep other
matter work moving while calendar access is being repaired.

## Check a proposed time

Before proposing or accepting a time, check the actual window with its UTC offset:

```bash
navigator calendar check --project <code> --host <host> --start <RFC3339> --end <RFC3339>
```

Proceed only when the required reads succeed and the check reports `clear: true`. A conflict or duplicate needs another
window. If a required read fails, report availability as unknown and pause scheduling until it succeeds.
