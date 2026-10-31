---
title: "Activity Log"
description: >-
  An Activity Log is a Section that renders a matter's activity as one dated entry per day, newest first.
---

A [Section](section.md) of a [Matter Page](matter-page.md): the [Matter](matter.md)'s activity as one dated entry per
day, newest first, built from the records already stored on the matter. It is the spine of the [Day Log](day-log.md)
page kind — the one section that page requires — and reads history rather than working it: there is no per-item status
to set and nothing firm-only to hide from a lens.

The vocabulary is closed: `rules::dashboard::Section` names it `activity_log`, and its component in `webapp::sections`
renders an empty state until the matter has records to show.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
