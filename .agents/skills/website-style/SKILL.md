---
name: website-style
description: >-
  Design or revise public websites with shared components and distinct brand fonts and colors. Use for website layouts,
  pricing, service directories, and visual consistency across brands.
---

# Website style

## Less is More

Feature the brand once in the page header. Start the content with its descriptive heading; omit repeated brand labels
above it, including on blog indexes and articles across Neon Law and Vesta Estate Planning. Prefer fewer elements and
less copy when they communicate the same meaning. Keep information needed for navigation, accessibility, legal
disclosures, and informed decisions.

Use descriptive headings and labels. Do not decorate website sections, cards, or features with `01`, `02`, or other
zero-padded numbers. Use ordinary ordered lists when sequence helps someone complete a task. Keep meaningful numbers
such as prices, dates, statutory references, and account identifiers intact.

Before designing, compare the affected sites and inspect the existing shared components and brand registry. Reuse common
structure, spacing, controls, responsive behavior, and accessibility conventions. Express each brand through its
registered font, palette, and logo. Preserve established visual motifs where they help recognition.

Use `webapp::components::pricing::PricingSection` for comparable plans and the shared marketing-page bands for service
directories. Resolve brand logos through `views::brand::BrandKey` and the public asset resolver; do not duplicate logos
or hard-code a second brand registry. Keep visitor-facing copy in `neon/locales/en/`.

Present the billing period and total price together. Put family-member prices and service-scope disclosures near the
offer. Check desktop and mobile layouts, contrast, keyboard access, and reduced-motion behavior using the task-owned
preview described in [web-preview](../web-preview/SKILL.md).

## Decoration

Apply Less is More to visual layers: let the primary illustration or logo carry the identity. Omit repeated logos,
background textures, heading ornaments, and drop shadows that add no information. Keep purposeful brand artwork, useful
section boundaries, interactive state indicators, and visible keyboard focus.
