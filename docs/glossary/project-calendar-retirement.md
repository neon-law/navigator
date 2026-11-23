---
title: "Project Calendar Retirement"
description: >-
  Project Calendar Retirement removes a closed or archived Project's shared calendar after its events reach PostgreSQL
  and a published Iceberg Archive.
---

The `project_calendar_retirement` table records the Firm, Project, calendar ID, final event versions, and content
digest. The [Iceberg Archive](iceberg-archive.md) generation identifies the published snapshot containing the event
[backups](project-calendar-event-backup.md).

DBOS rechecks the Project status, calendar ID, delegated owner, write access, and event inventory before requesting
deletion. A change to the event inventory starts another backup. The deletion intent survives an interrupted request.
Keep the delegated owner idle during the final check and delete request. Google versions the calendar and its events
separately. After deletion, Navigator clears the live calendar link and retains the receipt and iCalendar resources.
