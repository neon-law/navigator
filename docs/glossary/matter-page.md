---
title: "Matter Page"
description: >-
  A Matter Page is one page of a matter that a Project declares in its navigator.yaml and Navigator renders through each
  reader's own lens.
---

One page of a [Matter](matter.md), declared as a `pages:` entry in the Project [Repository](repository.md)'s
`navigator.yaml` and rendered by Navigator at `/app/projects/{code}/{slug}`. An entry names a URL `slug`, a page `kind`
(one of the matter page kinds, such as `docket_deadline_board` or `engagement_billing_records`), a `title`, an optional
`draft` path, and its `lenses:` — the [Section](section.md) list each [Lens](lens.md) shows, in order.

The page is composition, not code. The `kind` names a skeleton the gate checks the entry against (`Y021` for shape,
`D001` through `D005` for composition); the renderer only ever sees sections, one component each. A Markdown file never
declares a page: a frontmatter `kind:` that names a page kind fails `S103`.

`navigator project sync` stores the declared pages with the Project, replacing the whole set, so a render never reads
git. The reader's lens comes from their participation on the matter, re-checked on every request; a non-participant, an
unknown slug, and a page with no face for the reader are the same 404. The matter show page stays the first tab, and
each page with a face for the reader follows it in declared order.

- Rules: [`rules::pages`](../../rules/src/pages.rs) · Store: [`store::matter_pages`](../../store/src/matter_pages.rs) ·
  Schema: [`matter_page`](../../store/src/schema/navigator.surql) · Contract: [the
  gate](../gate.md#declaring-a-matter-page)

The table is `matter_page`.

Schema: [`matter_page`](../../store/src/schema/navigator.surql#L371).
