---
title: "Day Log"
description: >-
  A Day Log is a matter page kind that reads a matter's activity as one dated entry per day, newest first.
---

A [Matter Page](matter-page.md) kind that reads a [Matter](matter.md)'s history instead of working its deadlines: the
[Activity Log](activity-log.md) [Section](section.md) renders one dated entry per day, newest first, from the records
already stored on the matter. The `key_dates_strip` [Section](section.md) may ride along for orientation; nothing on the
page is firm-only, because reading what happened is not legal work.

The kind is declared in a Project's `navigator.yaml` as `kind: day_log` and validated against its skeleton in
`rules::dashboard` like every other [Matter Page](matter-page.md) kind.

- Vocabulary: [`rules::kind`](../../rules/src/kind.rs) · Skeleton: [`rules::dashboard`](../../rules/src/dashboard.rs)
