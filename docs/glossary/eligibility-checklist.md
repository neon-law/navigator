---
title: "Eligibility Checklist"
description: >-
  An Eligibility Checklist is a Section that carries what the client must satisfy for a matter's chosen path to stand.
---

A [Section](section.md) of a [Practice Guide](practice-guide.md): what the client must satisfy for the chosen path to
stand. It is one of the three sections that page requires — the plan's gates, stated so the person who must clear them
can read them and work.

The vocabulary is closed: `rules::dashboard::Section` names it `eligibility_checklist`, and its component in
`webapp::sections` renders an empty state until the matter's guide has gates to mark.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
