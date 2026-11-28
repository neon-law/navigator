---
title: "Portal Theme Brand"
description: "A Portal Theme Brand supplies the colours, logo, and name in the Firm team’s workbench."
---

A [Firm](firm.md) can select one of its [Brands](brand.md) for the portal its clerks, lawyers, admins, and owners use.
`firm.portal_theme_brand_key` holds the selected [Brand Key](brand-key.md). With no selection, each host wears its own
Brand. Clients use the Brand of the host where they sign in and see their Projects for that Brand.

A Brand that keeps its own portal look (`BrandKey::keeps_own_portal_look`, today Daybridge) skips the theme: its host's
`/app` pages wear its own tokens, logo, and name even when its Firm names a Portal Theme Brand.

The Firm must own the Brand (`brand.firm_id`). `store::firms::update` refuses any other key, and only a caller who may
edit the Firm sets it: Owner, or that Firm's own Admin (`FirmCapability::ManageMembership`). It is set on the Firm edit
page at `/app/admin/firms/{id}/edit` under **Base Firm URL**, or with `hudson site firm theme <slug> <brand-key>`
(`--clear` removes it).

The `/app` request layer (`webapp::app_chrome::firm_portal_theme_brand`) re-checks ownership on every request. A Brand
later moved to another Firm stops theming this Firm's portal, and the host's own Brand takes over. The theme changes
only the portal's look: the footer still names the Firm wearing the host's Brand. Only a compiled [House
Brand](house-brand.md) key themes the portal; a runtime-only Brand has no compiled logo or home to draw, so the host's
own Brand stays.

Firm overview, edit, and Finances pages use the selected Brand's website, logo, and tokens. The overview links to the
Brand's approved website URL or its registered public host. If no selected Brand has a website, these pages use the
first owned Brand with a website, ordered by name. An Owner changes an approved URL in Brand settings.

The column is `firm.portal_theme_brand_key`.

Schema: [`firm`](../../store/postgres/schema.sql).
