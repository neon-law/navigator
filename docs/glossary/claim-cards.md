---
title: "Claim Cards"
description: >-
  Claim Cards is a Section that carries one card per claim on a matter, each saying what it says and where it stands.
---

A [Section](section.md) of a [Litigation Pitch](litigation-pitch.md): one card per claim on the matter, each card saying
what the claim is and where it stands. It is one of the three sections that page requires, and like the rest of the
pitch it reads on the client's lens — a card keeps nothing from the person the claim belongs to.

The vocabulary is closed: `rules::dashboard::Section` names it `claim_cards`, and its component in `webapp::sections`
renders an empty state until the matter's pitch has claims to card.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Component:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
