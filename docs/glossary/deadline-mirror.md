---
title: "Deadline mirror"
description: "The Google Calendar events the nightly reconcile keeps aligned with a matter's Deadlines."
---

The one-to-one copy of a matter's [Deadline](deadline.md) rows as events on its shared matter
[Calendar](project-calendar.md), and the nightly reconcile that keeps the copy honest. A missed deadline is a
malpractice event, not a backlog item, so the date lives in two places on purpose: the [Project](project.md) row is the
record of what binds, and the calendar event is where the working day actually gets looked at. The mirror is what stops
the two from drifting apart silently.

The store's engine is `store::deadline_calendar`: it renders each open Deadline to one event on the ICS feed's field
basis (the title or its fallback, the due instant, the authority and deadline description, the internal lead days as the
only reminder), and marks every mirrored event with private extended properties — the Deadline's record id and a hash of
the rendered content — so the reconcile can tell drift apart from a stale cache. Completing or cancelling a Deadline
takes its event off the calendar as a cancelled event with no reminder. Deleting a Deadline row deletes its mirrored
event. An event a person moved or cancelled on the Google side is left alone: the reconcile writes from the rows, and it
never argues with a person's own calendar edits unless the row itself changes.

Reconciliation runs three ways, all through the same engine. The `navigator project deadline sync` verb drives the
portal's `POST /app/api/integrations/calendar/deadline-mirror` door for one matter; a nightly workflow at 04:03 UTC
walks every open matter under a durable execution, one checkpointed step per matter, so one unreachable calendar cannot
stall the night; and both accept a dry run that plans the inserts, patches, and deletions without making them. The
applied pass ensures the matter calendar exists and is shared before mirroring, and never invents a replacement for a
recorded calendar that has disappeared — a missing calendar is an operator-visible outcome, not a new blank one. The
nightly workflow is opt-in by deployment setting; live writes against Google Calendar additionally require domain-wide
delegation for the calendar scope, which is an account fact no repository carries.
