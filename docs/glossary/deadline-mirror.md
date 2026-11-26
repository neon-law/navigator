---
title: "Deadline mirror"
description: "The Google Calendar events the nightly reconcile keeps aligned with a matter's Deadlines."
---

The one-to-one copy of a matter's [Deadline](deadline.md) rows as events on its shared matter
[Calendar](project-calendar.md), and the nightly reconcile that keeps the copy honest. A missed deadline is a
malpractice event, not a backlog item, so the date lives in two places on purpose: the [Project](project.md) row is the
record of what binds, and the calendar event is where the working day actually gets looked at. The mirror is what stops
the two from drifting apart silently.

The mirror and ICS export put the Project code first, followed by a colon, a known category label, and the action. Civil
dates appear as all-day events. A supplied cutoff appears in the description. A hearing reserves Busy time for the
supplied duration, or one hour when its duration is unknown. The court-supplied address appears in Location. An open
timed Deadline holds one Busy minute so booking links skip that slot. Other Deadline events show as Free. Open court
Deadlines carry popup reminders seven, three, and one day before, plus any distinct internal lead. Google reminder
settings are personal to the calendar user.

The mirror records the Deadline ID, a hash of its source fields, and a fingerprint of the event Google returned after
the last write. Before updating or deleting the event, Navigator checks that fingerprint and sends Google the version it
just read. A direct edit or a concurrent change leaves the event in place and appears in the sync report for review. An
older event without a fingerprint also needs review when its Deadline changes. Events created directly on the calendar
stay outside the Deadline mirror.

For a verified event, completing its Deadline marks the title as complete and clears its reminders. Cancelling or
removing the Deadline clears its event. A sync with no changes leaves the event alone.

Reconciliation runs three ways, all through the same engine. The `navigator project deadline sync` verb drives the
portal's `POST /app/api/integrations/calendar/deadline-mirror` door for one matter; a nightly workflow at 04:03 UTC
walks every open matter under a durable execution, one checkpointed step per matter, so one unreachable calendar cannot
stall the night; and both accept a dry run that plans the inserts, patches, and deletions without making them. The
applied pass ensures the matter calendar exists and is shared before mirroring, and never invents a replacement for a
recorded calendar that has disappeared — a missing calendar is an operator-visible outcome, not a new blank one. The
nightly workflow is opt-in by deployment setting; live writes against Google Calendar additionally require domain-wide
delegation for the calendar scope, which is an account fact no repository carries.

Every outcome other than `mirrored` and `skipped_closed` means requested writes did not happen, and the verb exits
nonzero with the next step. `calendar_unprovisioned` says the matter has no recorded calendar and the provider refused
to create or adopt one; `provider_unauthorized` says Workspace refused the delegated owner; `provider_unavailable` is a
failure a retry clears; `mirror_incomplete` lists each refused row. A dry run of a matter with no calendar asks the
provider first, so a plan the provider would refuse fails the same way. A retry is always safe: each event carries its
Deadline id, so a second pass patches rather than duplicates.
