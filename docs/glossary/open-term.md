---
title: "Open Term"
description: "An Open Term is a {{question_code}} in a Draft that its questionnaire has not answered yet."
---

A `{{question_code}}` expression in a [Draft](draft.md), such as `{{person__client.name}}`, that the questionnaire has
not answered yet. The part before the first `.`, `|`, or space names the question that answers it. A rendered
[Notation](../notation.md#notation) highlights each open term and, when the page knows where that question lives, links
to it. An open term is filled from an answer, never typed into the document. Compare [Hold](hold.md), a decision the
lawyer still has to make.

- Parser: [`views::harvard_outline::runs`](../../views/src/harvard_outline.rs)
