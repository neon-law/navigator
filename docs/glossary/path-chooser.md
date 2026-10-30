---
title: "Path Chooser"
description: >-
  Path Chooser is a Section that states the path chosen for a matter, and why.
---

A [Section](section.md) of a [Practice Guide](practice-guide.md): the path chosen for the matter, and why. It is one of
the three sections that page requires — between the clocks the matter runs against and the eligibility gates on the
path, it is the plan's fork: what route the lawyer picked and the reasoning a client can read.

The vocabulary is closed: `rules::dashboard::Section` names it `path_chooser`, and its component in `webapp::sections`
renders an empty state until the matter's guide has a path to choose.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
