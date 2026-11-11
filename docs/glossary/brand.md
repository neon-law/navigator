---
title: "Brand"
description: "A Brand is a storefront identity a Firm owns, recorded as one row in the brand table."
---

A storefront identity one [Firm](firm.md) owns. **The `brand` table is the source of truth**
([`store::brands`](../../store/src/brands.rs)): one row per Brand, with a display `name`, a [Brand Key](brand-key.md),
the owning `firm_id`, and its presentation (colours, typeface, logo, font, website URL). A Firm's own Admin DRI creates
rows at runtime, so the set of Brands is open, not a fixed list. Which Firm *wears* a Brand, and so owns the Projects
filed under it, is recorded once in [Firm Brand](firm-brand.md).

Distinct from a [House Brand](house-brand.md), the compiled routing key that maps a request's `Host:` header to an
identity. Every House Brand has a `brand` row, seeded on first boot; a runtime row has no compiled House Brand, so it
routes no host and publishes no marketing page.

## Engagement identity

The Project manifest's `project.host` selects the Brand for the engagement. An optional `project.brand` asserts the same
identity; it cannot override the host-selected Brand or contradict the live Project. Use that identity for the
engagement's letterhead, questionnaire, preview and generated artifacts. The agreement, legal-party text and signature
identify the legal [Firm](firm.md) associated with the Brand and Project, even when its name differs from the Brand's
display name.

The CLI letterhead resolver uses the compiled [House Brand](house-brand.md) registry. It refuses a missing or unknown
host, an unknown declared key, or a declaration that disagrees with the host. A runtime Brand row alone supplies no
routable host or compiled letterhead. The live Project's Firm/Brand pair is checked separately against [Firm
Brand](firm-brand.md); a valid host does not grant ownership or Project access.

Keep an executed or filed artifact's original identity and bytes. Changing the current host or Brand does not authorize
restyling a signed engagement. New drafts use the current resolved identity; the signed artifact remains its own record.

## Stored presentation and administration

A row may carry a canonical **website URL**, `https://<host>`. Its host is the Brand's approved email domain, and the
set of those domains across every Brand is the only allowlist for account admission
([`store::brand_domains`](../../store/src/brand_domains.rs)); no second list exists. Only Owner sets or clears it, in
the Website form on `/app/admin/brands/{key}/edit` (`store::brands::set_website_url`; an Admin sees the approved URL
read-only): approving a domain is kept apart from host routing, from Brand ownership, and from role grants, so a Firm's
Admin DRI cannot widen admission. The stored form is canonical: `https` only, a lowercase IDNA (ASCII) host, no port
other than the default, no user info, path, query, or fragment, and never an IP address or a single label. A `www.` host
approves its apex (`https://www.example.com` approves `example.com`); any other subdomain approves exactly itself. An
email domain must equal an approved domain exactly, so neither a parent, a subdomain, nor a lookalike suffix is
admitted. A Brand without a URL, or with a stored value that is not canonical, approves nothing, and with no approved
domain no one is admitted. Each compiled key's row is seeded with `https://` plus its compiled `canonical_host()`; a
runtime Brand has none until Owner approves one, and a URL Owner clears is never refilled. The compiled keys migrate
into a Firm-scoped row on first boot — the practice Firm's own Admin DRI creates each one — so the one authorization
table names every brand a Firm may attach, while each compiled brand's hosts, fonts, logos, and copy stay with its
[House Brand](house-brand.md). A `firm_id IS NONE` row is a historical fact only. Every such row was backfilled onto the
Firm that wears it through `firm_brand`, or the anchor Firm when only a `project.brand` still named it, and the rest
were deleted. Nothing can write a new one.

Brand `name` and `brand_key` are each unique across the whole deployment, not per Firm: PostgreSQL holds
`brand.brand_key UNIQUE` and the `brand_name` unique index, and SurrealDB holds the `brand_key_unique` and `brand_name`
indexes. Both compare the exact stored value, with no case folding. Changing Firm ownership cannot make an existing name
or key available to another Firm.

A Brand is not a Forgejo namespace. Forgejo organizations belong to [Firms](firm.md): a Project's repository is
`<firm-slug>/<project-code>`, named from the owning Firm's immutable `firm.slug` and the Project's immutable `code`, so
renaming a Brand, Firm, or Project never moves a repository.

A Firm's own Admin DRI creates, edits, and deletes that Firm's `brand` rows at `/app/admin/brands`,
`/app/admin/brands/new`, and `/app/admin/brands/{key}/edit`; Owner governs every *existing* row the same way but creates
none — Owner holds no Firm membership, so Owner has no Firm to scope a new one to. `primary_color` is a free `#rrggbb`
hex, gated against the two fixed backgrounds it actually renders on: its deterministically-chosen on-primary text colour
(white or black, whichever contrasts more, not "the best of both") must clear WCAG AA 4.5:1, and the primary itself must
clear 3:1 against the light page surface — never a closed palette id. A row may also carry an uploaded logo (PNG or SVG,
sanitized against script content) and an uploaded `.woff2` font (attested under a closed open-licence list), both served
from the public assets bucket. New uploads use `brand-<Brand UUID>/` and store their exact object keys on the row;
existing references remain readable. The editor previews the built-in logo and configured typeface when no replacement
is attached. `typeface = "uploaded"` is what tells the tokens stylesheet to read the row's own font rather than a
compiled catalog entry. The Admin edit page shows existing logo and font references without upload or typeface-selection
boxes. Its reusable colour picker pairs a hex text field with a native swatch; the same server-side contrast checks gate
the saved value. The owning Firm link leads to the responsible people and shared social app-key workflow. Each Brand
connects its own social accounts. Asset uploads remain available through the authorized asset endpoints. Deleting a row
is refused while any `firm_brand` or `project.brand` value still names its key. None of this touches the compiled keys'
own served hosts, marketing pages, or fallback presentation. Editing the `neon` row changes the token stylesheet, not
which hosts resolve to it.

An uploaded logo also renders on `/app`, not only on the public site: `webapp::app_chrome::resolve_app_brand_mark`
prefers the resolved brand's `brand.logo_object_key` over the compiled `SiteBrand.logo_href` in the navbar mark,
resolved by the same portal-wide request layer that resolves [`FirmFooterModel`](firm-brand.md). The `/app` document
title is deliberately unaffected — every `/app/*` page's tab title still leads with "Navigator", because `/app` is the
firm's own internal tool rather than a client-facing surface a white-label deploy needs to rebrand in the reader's eyes.

The `/app` inventory surfaces name a compiled brand's production website as its `www` host (`www.deleteyourdata.com`):
`/app/admin/brands`, `/app/owner`, `/app/admin/firms/{id}`, the admin project directory, `/app/projects`, and
`/app/projects/{code}`. A compiled key that is not yet live still shows that host, labelled `not live`. A runtime-only
`brand` row has no public host. `project.brand` is written at matter-open from the request host unless an authorized
caller selects a Firm/brand pair, and `navigator project set --firm <name> --brand <key>` changes the pair later.

On the firm-side Projects list, the current Brand is selected by default. Select another Brand or **Firm** to change the
list scope. The lawyer Overview spans the Firm's Brands within the viewer's Project access. On **Edit project**, an
Owner or administering Firm Admin who participates on the firm side can change the Project's Brand within its Firm.

- Routing and presentation: [House Brand](house-brand.md)

The table is `brand`.

Schema: [`brand`](../../store/postgres/schema.sql).
