---
title: "Attorney Calendar"
description: "An Attorney Calendar is a Google Calendar id recorded on a Person for free/busy or their own events."
---

One Google Calendar id on a [Person](person.md). A lawyer-tier sign-in records the Person's Workspace address as the
`primary` calendar when none is recorded; a consumer Gmail address is never recorded. Attorney Calendars exist only for
a Firm whose [Firm Calendar Settings](firm-calendar-settings.md) name a Google Workspace mailbox. A Person can record
extra calendars as `freebusy`. Navigator also checks shared calendars from eligible open Project assignments in the
authorized Firm. Those calendars contribute busy intervals or access gaps without disclosing another Project's calendar
ID or titles. The Google OIDC subject on [Person External Identity](person-external-identity.md) identifies the Person
and is not a calendar id.

Navigator verifies the recorded calendar ID and Project code in the owning Firm's Workspace before checking shared
availability. A missing or mismatched Project Calendar makes the affected participant's availability unknown.

Free/busy for other Firm participants uses the Workspace delegation's free/busy scope and returns no event details.
Event details use the events scope and only for the signed-in Person's primary calendar. A calendar the delegation
cannot read is a gap that names the Person, the role, and the missing share or scope. A gap is never an empty busy list.

`navigator calendar` shows the signed-in attorney's primary events for the next 24 hours by default. `navigator calendar
access` checks that attorney's recorded calendars, and `navigator calendar freebusy` lists their busy blocks. `navigator
calendar check` exits non-zero when a calendar is unreadable, busy time overlaps, or primary events repeat a title.
`navigator calendar register` records an extra free/busy calendar or replaces the primary one.

Calendar coordinates stay off the Project participant list. A caller outside the Firm that owns the Workspace cannot
read them. The shared [Project Calendar](project-calendar.md) remains the matter calendar.

A booking registration is a separate private secondary Google Calendar. Create it in the attorney's Workspace account
with the description `Navigator Booking Availability (production)` or `Navigator Booking Availability (staging)` for
that environment. Register its Calendar ID with `navigator calendar register --booking --calendar <id>`. In the
attorney's existing Notion Calendar scheduling link, enable **Avoid conflicts** and select that secondary calendar once.

The DBOS reconciliation reads the timed Busy intervals on the attorney's eligible open Project Calendars for the next 90
days. It writes private `Busy` holds without guests, reminders, or matter details and removes obsolete holds it can
verify it owns. An open Project joins the sync when its Calendar ID is recorded. A Lawyer assignment still contributes
private Busy holds after promotion to Admin or Owner when the Person qualifies through the Firm's current membership.
Unreadable or unverified recorded calendars leave existing holds in place. The booking calendar stays out of Navigator's
own availability reads.

The table is `person_calendar`.

Schema: [`person_calendar`](../../store/postgres/schema.sql).
