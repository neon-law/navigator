---
title: Time Entry
description: >-
  One attorney's attributed block of work on a matter — minutes, billable
  flag, and the hourly rate captured at creation.
---

A **Time Entry** is one [attorney's](person.md) block of work on a [Project](project.md), recorded in whole minutes and
attributed to the person who performed it. A billable entry freezes the hourly rate that priced it
(`rate_cents_per_hour`) at creation, so later [Fee Structure](fee-structure.md) edits never reprice captured work — the
same discipline [Notation Meter Entries](notation-meter-entry.md) apply to quoted work. Amounts derive in integer cents,
never floats. An entry is logged by hand or captured when a [Timer](timer.md) stops, and it is never deleted: billing
resolves it — unbilled, invoiced under one [Xero Invoice](xero-invoice.md), or written off with a reason — and every
transition appends an audit event naming the actor.

A Time Entry is not a [Deadline](deadline.md): a deadline is a docket obligation whose miss is a malpractice event,
while an entry is the record of work already performed.

- Vocabulary: [`store::time_entries`](../../store/src/time_entries.rs)
