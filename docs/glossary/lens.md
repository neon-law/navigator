---
title: "Lens"
description: >-
  A Lens is the face of a Matter Page a reader sees, client, lawyer, or clerk, chosen by their participation on the
  matter.
---

The face of a [Matter Page](matter-page.md) one reader sees. There are three: `client`, `lawyer`, and `clerk`. A page
declares the [Section](section.md) list for each lens it has in one `pages:` entry, so the faces of a page cannot drift
apart across separate documents.

The lens is a fact about the reader, never about the URL. It comes from their [Participation](participation.md) on the
matter, through the same `store::access::matter_viewer` answer the matter show page dispatches on: a client-side
participant reads the client lens, a firm-side participant the lawyer lens, and a supervised clerk the clerk lens. A
page that declares no face for the reader does not exist for them.

A lens composes; it does not authorize. A page kind marks the sections that are the firm's working surface — a prebill
review, a reviewer's status setter — and `D005` keeps those on the lawyer lens, but each section still loads only the
rows its reader may see. A clerk lens loads no documents, deadlines, or billing.

- Vocabulary: [`rules::dashboard::Lens`](../../rules/src/dashboard.rs) · Route:
  [`webapp::sections::page`](../../webapp/src/sections/page.rs)
