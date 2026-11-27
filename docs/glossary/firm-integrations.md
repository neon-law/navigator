---
title: "Firm Integrations"
description: "Firm Integrations are the provider choices and settings shared by a Firm's Projects."
---

On the [Firm](firm.md) overview, an Owner or the Firm's Admin selects Google Workspace or Microsoft 365, and DocuSign or
Dropbox Sign. Notion and Slack show whether the Firm has stored credentials. A Firm's Admin DRI manages those
credentials.

Google Workspace is the default suite. Selecting Microsoft 365 pauses Google Calendar and Drive for the Firm's
[Projects](project.md) and retains their Google settings. Microsoft calendar and file sync are unavailable.

With Google Workspace selected, an Owner can configure the following on the Firm edit page.

- **Google Calendar on:** [Firm Calendar Settings](firm-calendar-settings.md) name the Workspace mailbox that owns each
  Project's [Project Calendar](project-calendar.md).
- **Google Calendar off:** Calendar and booking links disappear from Project pages. Dates live as
  [Deadlines](deadline.md) and travel as ICS files.
- **Google Drive on:** each Project carries a private Drive folder, and the Project page links it.
- **Google Drive off:** Drive links and the Drive folder setting disappear from Project pages. A recorded folder id
  stays stored.

A Firm without a saved Drive choice has Drive off. [G&S](gs.md) has both on.

`navigator project status <code> --json` reports the owning Firm's effective Google Calendar and Drive switches as
`firm_integrations`, read inside the caller's own matter scope.

The Calendar switch is the presence of a mailbox in `firm_calendar_settings`. The Drive switch is its own table.

The table is `firm_drive_settings`.

Schema: [`firm_drive_settings`](../../store/postgres/schema.sql).
