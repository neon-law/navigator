---
title: "Hold"
description: "A Hold is a paragraph of a Draft written wholly in square brackets: a drafting decision still open."
---

A paragraph of a [Draft](draft.md) written wholly in square brackets, such as `[Confirm the hearing date.]`. It parks a
drafting decision: the document is not ready to leave the firm while one remains. `[blank]`, `[...]`, and `[…]` are
fill-in blanks, not holds, and a bracketed link is prose. A hold is flagged in words, not only in colour, wherever a
[Notation](../notation.md#notation) renders, and each one is a step of a Matter Page's `notation_checklist`. A hold
whose text starts `Resolved.` is a checked step. Not a [Legal Hold](legal-hold.md), which keeps a document out of the
retention purge.

- Parser: [`views::harvard_outline::hold_body`](../../views/src/harvard_outline.rs)
