---
title: Letterhead
description: "The mark and contact lines a Brand frames its letters with, set on the Brand's edit page."
---

A **Letterhead** is what heads every [Letter](letter.md) a [Brand](brand.md) sends: its wordmark or uploaded logo, the
legal name of the [Firm](firm.md) that owns the Brand, one contact line (phone, email, site), the postal
[Address](address.md), and the font the lines are set in. Each Brand carries its own.

An Owner, or the Admin DRI of the Brand's own Firm, sets the runtime letterhead fields on the Brand's edit page at
`/app/admin/brands/<key>/edit`. Every line is optional and falls back to the Brand itself: an unset wordmark is the
Brand's name, an unset site is the Brand's approved website, and an unset email is `contact@` that website's domain. The
Firm's legal name always comes from the Brand's Firm, never from the form. The font is one of the Brand's own uploaded
fonts, or the renderer's default face.

Guieb & Shook letters use the navy wordmark, Instrument Sans and the address for the letter's jurisdiction. The
California, Nevada, New York and Washington offices each have a letterhead. Reno is the default. The visible e-signature
fields follow the letterhead's colors and retain their provider anchors.

[`store::brand_letterheads`](../../store/src/brand_letterheads.rs) resolves one runtime row per Brand.

The table is `brand_letterhead`.
