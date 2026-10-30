---
title: "Filing"
description: "A Filing records one outbound compliance submission for a Notation."
---

A stored Filing records one outbound compliance submission for a [Notation](notation.md), such as mail to a party or a
submission to a government office. It preserves what was submitted, where, and when.

The workflow prefix `filing` records a named government filing. It is an outbound submission step and must sit behind
[Lawyer Review](lawyer-review.md). See the
[`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry) guide and
[`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).

The table is `filing`.

Schema: [`filing`](../../store/src/schema/navigator.surql#L2042).
