---
title: "Signature"
description: >-
  A Signature records a signing request or execution on a Notation's document.
---

A stored Signature is one signing request or execution on a [Notation](notation.md)'s document. Its provider reference
connects later signing results to the same request, and its state records the outcome.

The workflow suffix family `_signature` / `_signatures`, plus the `witnesses` prefix, records respondent-side signing.
See [`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry) and
[`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).

The table is `signature`.

Schema: [`signature`](../../store/src/schema/navigator.surql#L2076).
