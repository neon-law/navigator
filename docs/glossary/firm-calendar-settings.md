---
title: "Firm Calendar Settings"
description: >
  Firm Calendar Settings select the Google Workspace mailbox that owns a Firm's Project Calendars and make Google the
  Firm's main email.
---

In [Firm](firm.md) Settings, an Owner chooses the mailbox that owns the Firm's [Project Calendars](project-calendar.md).
Saving a Google Workspace mailbox is the choice of Google as the Firm's main email, and Google Calendar exists only for
a Firm that made it: Project Calendar provisioning and every [Attorney Calendar](attorney-calendar.md) read refuse a
Firm without it. Consumer Gmail addresses are refused because they cannot grant Workspace delegation. Clearing the
mailbox, or switching Calendar off in [Firm Integrations](firm-integrations.md), turns Calendar off.

The table is `firm_calendar_settings`.

Schema: [`firm_calendar_settings`](../../store/postgres/schema.sql).
