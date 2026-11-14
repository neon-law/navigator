---
title: "Portal Theme Brand"
description: "A Portal Theme Brand is the one Brand a Firm owns whose look its authenticated portal wears."
---

The one [Brand](brand.md) a [Firm](firm.md) picks for its authenticated portal. `firm.portal_theme_brand_key` holds that
Brand's [Brand Key](brand-key.md), or nothing. Every `/app` page reached through a host whose Brand the Firm wears then
draws that Brand's tokens stylesheet, logo, and name, whichever of the Firm's Brands the host resolved to. The portal
looks like the client's firm, not like the storefront the login came through. With no Portal Theme Brand set, each host
keeps its own Brand's look.

The Firm must own the Brand (`brand.firm_id`). `store::firms::update` refuses any other key, and only a caller who may
edit the Firm sets it: Owner, or that Firm's own Admin (`FirmCapability::ManageMembership`). It is set on the Firm edit
page at `/app/admin/firms/{id}/edit`, or with `hudson site firm theme <slug> <brand-key>` (`--clear` removes it).

The `/app` request layer (`webapp::app_chrome::firm_portal_theme_brand`) re-checks ownership on every request. A Brand
later moved to another Firm stops theming this Firm's portal, and the host's own Brand takes over. The theme changes
only the portal's look: the footer still names the Firm wearing the host's Brand. Only a compiled [House
Brand](house-brand.md) key themes the portal; a runtime-only Brand has no compiled logo or home to draw, so the host's
own Brand stays.

The column is `firm.portal_theme_brand_key`.

Schema: [`firm`](../../store/postgres/schema.sql).
