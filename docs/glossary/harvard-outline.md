---
title: "Harvard Outline"
description: "A Harvard Outline numbers motion and contract provisions so each can be addressed by path."
---

The numbering used on motions and contracts so a provision is addressable by path (`I`, `I.A`, `1.B`, `1.1(a)`). An
agreement keeps one scheme for the whole body: Roman numerals (`I.`, `II.`), decimal numbers (`1.`, `1.1`), or the
Section convention (`Section 1.01`). Closing letters and wills stay Roman. Pleadings use Arabic numerals (`1.`, `2.`).
Roman outlines and pleadings letter their subsections (`A.`, `B.`) as Markdown block quotes. Decimal and Section
agreements use `(a)`, `(b)`, `(c)` the same way (`> **(a) Label.**`). `views::harvard_outline` parses the Roman and
lettered shape into narration units. A notation the firm has given a client narrates on that stage at
`/app/projects/{code}/{notation_id}/outline`. An uncaptioned provision is a heading that carries only its marker (`##
I`, `### A`). `N123` holds every kind that must carry an outline to its scheme. In the rendered PDF each depth steps in
one indent from its parent: a lettered subsection hangs its marker (`A.`), and a numbered item under it starts beneath
that subsection's text, so wrapped lines align to the text after the marker.

- Parser: [`views::harvard_outline`](../../views/src/harvard_outline.rs)
- How to author: [`notation-authoring`](../notation-authoring.md#harvard-outline)
