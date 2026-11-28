---
title: Time Entry Proposal
description: Time a lawyer recorded or Navigator proposed from a Project's record, waiting for approval as a Time Entry.
---

A **Time Entry Proposal** is one block of work on a [Project](project.md) that is not yet a priced [Time
Entry](time-entry.md). A lawyer records one with `navigator project time add`, or `navigator project time backfill`
proposes one from a dated record: a [Communication](communication.md), a filed document, a completed
[Deadline](deadline.md), or an e-filing. It names the timekeeper, the day of the work, whole minutes, and a narrative.
Work dated before the day it was recorded is **backfilled**; the proposal keeps its insertion time.

A proposal prices nothing. The lawyer edits it, then approves or discards it. Approval resolves the rate the way review
of a [Billing Annotation](billing-annotation.md) does and creates the Time Entry in the same transaction: billable, no
charge, or linked to a [Legal Plan Claim](legal-plan-claim.md). The reviewed proposal is retained with its decision. A
source record is proposed at most once, so a repeated backfill adds nothing.

The table is `time_entry_proposal`. See [Record time from the CLI](../legal-plans.md#record-time-from-the-cli).
