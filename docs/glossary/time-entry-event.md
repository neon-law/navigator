---
title: "Time Entry Event"
description: "An immutable audit entry for a Time Entry's creation or billing transition."
---

An event names its [Time Entry](time-entry.md), actor, event kind, and before and after values. Reservations, invoices,
and write-offs retain their recorded sequence without deleting the work entry.

The table is `time_entry_events`.

Schema: [`time_entry_events`](../../store/postgres/schema.sql).
