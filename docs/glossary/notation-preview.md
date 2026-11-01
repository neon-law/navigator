---
title: "Notation Preview"
description: >-
  A stored, expiring draft of a Template, backed by a real Notation run.
---

A short-lived stored draft of a [Template](template.md) in a [Project](project.md). Pushing it persists the pushed
source as an immutable project-scoped Template version, enforces the [engagement-first rule](engagement--retainer.md),
and opens a real running [Notation](notation.md) and workflow journal — the preview's answerable questionnaire, which
the draft page hands off to. A draft whose push predates the run link, or whose run could not open, is stored without a
Notation. A local author preview (`navigator notation preview --offline`) is dry by design: it opens no run at all.

The table is `notation_draft`.

Schema: [`notation_draft`](../../store/src/schema/navigator.surql#L1890).
