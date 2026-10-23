---
title: "Section"
description: >-
  A Section is one registered building block of a Matter Page, such as a deadline board or an invoice, rendered by its
  own component.
---

One building block of a [Matter Page](matter-page.md): a boundary note, a deadline board, a list of files, an invoice,
and so on. The vocabulary is closed — `rules::dashboard::Section` names every one — and each page kind carries a catalog
of the sections it may use, with a spine it must use. A section outside the vocabulary fails `D001`; one outside the
kind's catalog fails `D002`.

Two sections are universal. `boundary_note` says what the page is not and what still needs a human, and
`provenance_statement` says as of when and from what records the page was built. Every [Lens](lens.md) of every page
carries both (`D003`).

Each section maps to exactly one component in `webapp::sections`, through an exhaustive match, so a new section does not
compile until it has a component. Every component has an empty state, and each section loads its data scoped to the
reader's lens.

- Vocabulary: [`rules::dashboard`](../../rules/src/dashboard.rs) · Components:
  [`webapp::sections`](../../webapp/src/sections/mod.rs)
