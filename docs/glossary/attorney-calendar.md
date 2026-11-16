---
title: "Attorney Calendar"
description: "An Attorney Calendar is a Google Calendar id recorded on a Person for free/busy or their own events."
---

One Google Calendar id on a [Person](person.md). A lawyer-tier sign-in records the Person's Workspace address as the
`primary` calendar when none is recorded. An extra calendar is `freebusy` and exists only after that Person records it.
The Google OIDC subject on [Person External Identity](person-external-identity.md) identifies the Person and is not a
calendar id.

Free/busy for other Firm participants uses the Workspace delegation's free/busy scope and returns no event details.
Event details use the events scope and only for the signed-in Person's primary calendar. A calendar the delegation
cannot read is a gap that names the Person, the role, and the missing share or scope. A gap is never an empty busy list.

`navigator calendar access` reports each attorney. `navigator calendar freebusy` returns busy blocks for the Firm
attorneys on a Project, or for the caller's Firms when no Project is named. `navigator calendar events` without a
Project reads the signed-in attorney's events. `navigator calendar check` exits non-zero when a required calendar is
unreadable, when busy time or a [Deadline](deadline.md) overlaps the window, or when that attorney's events repeat a
title. `navigator calendar register` records an extra free/busy calendar or replaces the primary calendar.

Calendar coordinates stay off the Project participant list. A caller outside the Firm that owns the Workspace cannot
read them. The shared [Project Calendar](project-calendar.md) remains the matter calendar.

The table is `person_calendar`.

Schema: [`person_calendar`](../../store/postgres/schema.sql).
