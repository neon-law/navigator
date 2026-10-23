---
name: design-mockup-translation
description: Translate an approved external design into Navigator's Dioxus surface.
---

# Translate a design mockup

Read [`docs/design-mockups.md`](../../../docs/design-mockups.md),
[`docs/access-model.md`](../../../docs/access-model.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- Port the intended layout, states, and copy through the existing Dioxus components. Do not merge external prototype
  markup or add a client-side authorization decision.
- Build every named state, keep authorization server-side, add an SSR test, and use a browser check when needed.
- Design references and captures must be synthetic or firm-owned and contain no client data, legal files, real contact
  details, or production identifiers.

A Project's client-facing screens are [Matter Pages](../../../docs/glossary/matter-page.md): a `pages:` entry in its
`navigator.yaml` composes [Sections](../../../docs/glossary/section.md), and the section registry in
[`webapp/src/sections/mod.rs`](../../../webapp/src/sections/mod.rs) renders each one. A mockup for a matter screen
becomes a page declaration over existing sections, or a new section added to that registry, which fails to compile until
`load` gives it a view and `SectionBody` renders it. Never a per-Project front end.
