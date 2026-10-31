---
title: "Rule Clocks"
description: >-
  Rule Clocks is a Section that carries each deadline clock a matter runs against, and how close it is.
---

A [Section](section.md) of a [Practice Guide](practice-guide.md): each clock the matter runs against, and how close it
is. It is one of the three sections that page requires — the plan starts from what is running out, so the lawyer works
the nearest clock first.

The vocabulary is closed: `rules::dashboard::Section` names it `rule_clocks`, and its component in `webapp::sections`
renders an empty state until the matter's guide has clocks to show.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
