---
title: "Notarization"
description: >-
  A Notarization records a provider request or execution for a Notation's document.
---

A stored Notarization is one provider request or execution for a [Notation](notation.md)'s document. The provider
reference connects its result to the request; an executed Document can also carry its notarial acknowledgments.

The workflow prefix `notarization` records a respondent signing or refusing in front of a notary. See
[`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry) and
[`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).

On an executed [Document](document.md), `notarization` is the optional list of notarial acknowledgments in the packet,
stored on the `assets` row and its committed pointer. Each entry records `method` (`in_person` or `remote_online`),
`acknowledged_on`, the two-letter `state`, the `county`, and `commission_expires`. It never records the notary's name or
commission number. It is recorded at upload with a repeatable `navigator site document upload --notarization` flag, and
no gate requires it.

- Shape: [`store::documents::Notarization`](../../store/src/documents.rs)

The table is `notarization`.

Schema: [`notarization`](../../store/src/schema/navigator.surql#L2104).
