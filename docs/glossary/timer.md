---
title: Timer
description: >-
  A durable running clock on an attorney's work; stopping it captures a Time
  Entry in the same transaction.
---

A **Timer** is an attorney's running clock on a [Project](project.md), persisted in PostgreSQL the moment it starts —
the page script only animates the display; the table row is the source of truth. A firm holds at most one open Timer
per attorney, enforced by the database, so starting a second requires stopping or discarding the first. Stopping
captures the elapsed whole minutes as a [Time Entry](time-entry.md) in one
transaction; discarding removes the row and records nothing. A Timer left running past one day is refused: the
attorney discards it or logs the time by hand.

- Vocabulary: [`store::timers`](../../store/src/timers.rs)
