---
title: "Litigation Pitch"
description: >-
  A Litigation Pitch is a matter page kind that pitches a matter's claims and their cost on one page.
---

A [Matter Page](matter-page.md) kind that pitches a matter: the [Pitch Headline](pitch-headline.md) states the claim in
one line a client can read, the [Claim Cards](claim-cards.md) carry one card per claim, and the [Fee
Summary](fee-summary.md) says what pursuing them costs. A pitch hides nothing from the person being pitched — no section
on the page is firm-only.

The kind is declared in a Project's `navigator.yaml` as `kind: litigation_pitch` and validated against its skeleton in
`rules::dashboard` like every other [Matter Page](matter-page.md) kind.

- Vocabulary: [`rules::kind`](../../rules/src/kind.rs) · Skeleton: [`rules::dashboard`](../../rules/src/dashboard.rs)
