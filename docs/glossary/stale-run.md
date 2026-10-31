---
title: "Stale Run"
description: >-
  An open Notation run whose pinned Template digest has been overtaken by the Template's current definition on main.
---

A started [Notation](notation.md) whose pinned Template digest differs from the Template's current definition on the
repository's main. Staleness is proven only when both digests are known: a run opened before per-run digests existed
still resolves through the store-Template binding it was opened against, and a Template removed from main is its own
lifecycle, not a stale run. The notation board's STALE column flags such runs, and `navigator project notations` refuses
its pre-flight while one is open.

No table of its own — staleness is computed for the `notation` run and its Template.
