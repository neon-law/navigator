---
title: "Fee Summary"
description: >-
  A Fee Summary is a Section that says what pursuing a matter's claims costs, on the reader's lens.
---

A [Section](section.md) of a [Litigation Pitch](litigation-pitch.md): what pursuing the matter's claims costs, shown on
the reader's lens. It is one of the three sections that page requires — the pitch states the claim and its posture, and
this section puts a number beside it, so the person being pitched can weigh both.

The vocabulary is closed: `rules::dashboard::Section` names it `fee_summary`, and its component in `webapp::sections`
renders an empty state until the matter's pitch has fees to summarize.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
