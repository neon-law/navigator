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

Every footer names the Firm actually wearing the request's resolved brand, not a compiled constant:
`webapp::firm_footer::resolve_firm_footer_model` reads the Firm's Entity for the legal name and `firm_brand` for the
"Our Family" row, in registry order with the current brand flagged, falling back to the compiled family
(`views::brand::firm_family`) filtered to reachable live brands plus the current brand only when no Firm wears the key —
so the row is on every page from first boot without advertising an unopened host. The `/app` footer
(`webapp::firm_footer::FirmFooter`) and the public chrome's footer draw from this one resolved model rather than
duplicating the lookup or rendering a second Firm's brands. Both also carry the firm's association memberships
(`views::brand::firm_memberships`) as one "Proud member of the …" line each, linking the association's own site; a
white-label bundle that renames the firm publishes neither row, because the family and the membership are Guieb & Shook
PC's.

- Schema: [`firm_brand` in the PostgreSQL schema](../../store/postgres/schema.sql) ·
  [`store::firms`](../../store/src/firms.rs)

The table is `firm_brand`.

Schema: [`firm_brand`](../../store/postgres/schema.sql).
