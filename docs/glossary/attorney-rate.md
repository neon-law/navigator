---
title: Attorney Rate
description: >-
  One attorney's effective-dated hourly price, firm-wide or as a per-matter
  override.
---

An **Attorney Rate** is the hourly price of one [attorney's](person.md) work — either the firm default (`project_id`
unset) or a per-[matter](project.md) override. Rows are effective-dated and insert-only: a rate change is a new row
with a later `effective_on`, never an edit, so the table is its own history. Resolution prefers the matter override,
then the firm default, taking the latest row effective on or before the date asked. [Time Entries](time-entry.md)
freeze the rate they were captured under, so edits here never reprice recorded work. Part of a [Fee
Structure](fee-structure.md).

- Vocabulary: [`store::attorney_rates`](../../store/src/attorney_rates.rs)
