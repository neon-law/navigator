---
title: "Firm Brand"
description: "A Firm Brand records that one Firm wears one Brand, the link Project ownership is checked against."
---

Which [Brands](brand.md) a [Firm](firm.md) wears. **The `firm_brand` table is the source of truth for wearing**: a row
holds `firm_id`, a [Brand Key](brand-key.md), and timestamps, unique on the pair and unique on `brand_key` across the
whole deployment, so one key is worn by at most one Firm and a Brand Key implies its Firm. Project ownership reads this
table: `store::firms::firm_id_for_brand_key` resolves the Firm for a key, and a Project's `firm_id` must be that Firm.

Distinct from `brand.firm_id`, which records which Firm *owns* the Brand row and so whose Admin DRI may edit it. The two
normally agree, and the attach paths keep them so: a Firm wears a Brand it creates at `/app/admin/brands/new` at once,
and a Brand row its Firm does not yet wear shows an "Attach to Firm" button on its edit page, which calls
`store::firms::attach_own_brand` (Owner, or that Firm's Admin), reading the Firm from the `brand` row, never from the
form, so no Firm can wear another Firm's Brand that way. Owner's `store::firms::attach_brand` names its Firm directly.

`store::firms::attach_brand` validates `brand_key` against the `brand` table (`store::brands`): a live row carrying that
key. A Firm may therefore wear any Brand a row names, runtime-created ones included, not only the compiled [House
Brands](house-brand.md). `store::seed` migrates each House Brand into a `brand` row on first boot
(`store::firms::CLOSED_BRAND_KEYS` is the compiled list it reads) so validation has a catalog from the start.

Every footer names the Firm wearing the request's resolved Brand. The footer resolver reads its Entity for the legal
name and links the Firm's own home when it wears its compiled firm Brand. When no Firm wears the key, the compiled
Branding supplies both. Footers do not list the Firm's other Brands.

Every Guieb & Shook public footer carries the same twelve page links. Design, Blog, Privacy, Terms, and Testimonials
stay on the current Brand's host. Shared tools, including API, link to Neon's host. The public footer also carries the
Firm's association memberships. The `/app` footer uses the same resolved legal name in a compact copyright line. A
renamed operator inherits its own identity and attached Brands.

- Schema: [`firm_brand` in the PostgreSQL schema](../../store/postgres/schema.sql) ·
  [`store::firms`](../../store/src/firms.rs)

The table is `firm_brand`.

Schema: [`firm_brand`](../../store/postgres/schema.sql).
