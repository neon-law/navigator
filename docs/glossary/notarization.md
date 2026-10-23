---
title: "Notarization"
description: >-
  The workflow prefix notarization records a respondent signing or refusing in front of a notary; on a Document, it is
  the list of notarial acknowledgments.
---

The workflow prefix `notarization` records a respondent signing or refusing in front of a notary. See
[`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry) and
[`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).

On an executed [Document](document.md), `notarization` is the optional list of notarial acknowledgments in the packet,
stored on the `assets` row and its committed pointer. Each entry records `method` (`in_person` or `remote_online`),
`acknowledged_on`, the two-letter `state`, the `county`, and `commission_expires`. It never records the notary's name or
commission number. It is recorded at upload with a repeatable `navigator site document upload --notarization` flag, and
no gate requires it.

- Shape: [`store::documents::Notarization`](../../store/src/documents.rs)
