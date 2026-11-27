---
title: "Firm Integrations"
description: "Firm Integrations are an Owner's On/Off switches for Google Calendar and Google Drive on a Firm."
---

On the [Firm](firm.md) edit page, an Owner switches Google Calendar and Google Drive on or off for every
[Project](project.md) the Firm owns.

- **Google Calendar on:** [Firm Calendar Settings](firm-calendar-settings.md) name the Workspace mailbox that owns each
  Project's [Project Calendar](project-calendar.md).
- **Google Calendar off:** Calendar and booking links disappear from Project pages. Dates live as
  [Deadlines](deadline.md) and travel as ICS files.
- **Google Drive on:** each Project carries a private Drive folder, and the Project page links it.
- **Google Drive off:** Drive links and the Drive folder setting disappear from Project pages. A recorded folder id
  stays stored.

A Firm without a saved Drive choice has Drive off. [G&S](gs.md) has both on.

`navigator project status <code> --json` reports the owning Firm's switches as `firm_integrations`, read inside the
caller's own matter scope.

The Calendar switch is the presence of a mailbox in `firm_calendar_settings`. The Drive switch is its own table.

The table is `firm_drive_settings`.

Schema: [`firm_drive_settings`](../../store/postgres/schema.sql).
