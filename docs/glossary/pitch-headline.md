---
title: "Pitch Headline"
description: >-
  A Pitch Headline is a Section that states a matter's claim in one line a client can read.
---

A [Section](section.md) of a [Litigation Pitch](litigation-pitch.md): the matter's claim, in one line a client can read.
It is one of the three sections that page requires — the headline the rest of the pitch expands on, written in plain
language rather than pleading captions.

The vocabulary is closed: `rules::dashboard::Section` names it `pitch_headline`, and its component in `webapp::sections`
renders an empty state until the matter's pitch has a headline to show.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
