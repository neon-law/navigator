---
title: "Project Calendar Event Backup"
description: >-
  A Project Calendar Event Backup preserves one Google Calendar event resource revision in iCalendar format.
---

Navigator reads event resources from the private [Project Calendar](project-calendar.md) using the Firm's delegated
owner. Each resource includes its recurring events, exceptions, and time zone definitions as supplied by Google.
PostgreSQL keeps each observed revision in `project_calendar_event_backup`, keyed by Firm, Project, calendar, resource,
and provider ETag. The [Iceberg Archive](iceberg-archive.md) exports the stored revisions.
