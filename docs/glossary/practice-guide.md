---
title: "Practice Guide"
description: >-
  A Practice Guide is a matter page kind: the lawyer's working plan for a matter, on one page a client can read.
---

A [Matter Page](matter-page.md) kind that is a matter's working plan: the [Rule Clocks](rule-clocks.md) it runs against,
the [Path Chooser](path-chooser.md) state of the route chosen for it, and the [Eligibility
Checklist](eligibility-checklist.md) of what the client must satisfy for that path to stand. A guide is a plan the
lawyer works from — and, like every [Matter Page](matter-page.md), one the client can read.

The kind is declared in a Project's `navigator.yaml` as `kind: practice_guide` and validated against its skeleton in
`rules::dashboard` like every other [Matter Page](matter-page.md) kind.

- Vocabulary: [`rules::kind`](../../rules/src/kind.rs) · Skeleton: [`rules::dashboard`](../../rules/src/dashboard.rs)
