---
title: "Brand"
description: "A Brand is a registered house identity with its own branding configuration."
---

A closed key naming a house brand — [`views::brand::BrandKey`](../../views/src/brand.rs): (`neon`, `delete-your-data`,
`lawyer-shook`, `vesta`, `misericordia`, `abhaya`, `delete-your-debt`, `summons`, `daybridge`, `death-and-divorce`,
`cyber-injury-law`, `aegis`, `groundwork`). **A brand is a registry entry, not a binary.** Each key resolves its own
[`Branding`](../../views/src/brand.rs). One `neon-server` process serves every compiled key; its
[`registered_brand_key`](../../views/src/brand.rs) resolver maps each request's `Host:` header to a key. One repository,
one running process, N house brands — adding one is a code change to the registry (a new key, its hosts, its `Branding`)
with a covering test, which is the right cost for a legal identity, and there is no runtime flag that can move a page
from one brand's hosts to another's.

**Distinct from the data-driven `brand` table** (`store::brands`) — a name, a unique key, and an authorization/identity
record, not a routing registry entry. Every row is Firm-scoped: `firm_id` is required, created only by that Firm's own
Admin DRI. It routes no host: `hosts()` and `registered_brand_key` keep resolving only the compiled `BrandKey` enum
above, and a runtime `brand` row publishes no marketing page.

A row may carry a canonical **website URL**, `https://<host>`. Its host is the Brand's approved email domain, and the
set of those domains across every Brand is the only allowlist for account admission
([`store::brand_domains`](../../store/src/brand_domains.rs)); no second list exists. Only Owner sets or clears it
(`store::brands::set_website_url`): approving a domain is kept apart from host routing, from Brand ownership, and from
role grants, so a Firm's Admin DRI cannot widen admission. The stored form is canonical: `https` only, a lowercase IDNA
(ASCII) host, no port other than the default, no user info, path, query, or fragment, and never an IP address or a
single label. A `www.` host approves its apex (`https://www.example.com` approves `example.com`); any other subdomain
approves exactly itself. An email domain must equal an approved domain exactly, so neither a parent, a subdomain, nor a
lookalike suffix is admitted. A Brand without a URL, or with a stored value that is not canonical, approves nothing, and
with no approved domain no one is admitted. Each compiled key's row is seeded with `https://` plus its compiled
`canonical_host()`; a runtime Brand has none until Owner approves one, and a URL Owner clears is never refilled. The
compiled keys migrate into a Firm-scoped row on first boot — the practice Firm's own Admin DRI creates each one — so the
one authorization table names every brand a Firm may attach, while each brand's hosts, colours, fonts, logos, and copy
stay with this entry. A `firm_id IS NONE` row is a historical fact only. Every such row was backfilled onto the Firm
that wears it through `firm_brand`, or the anchor Firm when only a `project.brand` still named it, and the rest were
deleted. Nothing can write a new one.

Brand `name` and `brand_key` are globally unique across Firms. Today this is enforced by separate unique indexes on the
exact stored values; changing Firm ownership cannot make an existing name or key available to another Firm. The
PostgreSQL migration must preserve that global scope and first audit existing values for collisions before adding its
constraints. It must state and test any canonicalization policy explicitly; a database's default locale or collation
must not silently change which values collide.

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
`brand` row has no public host. `project.brand` is still written at matter-open from the request host and cannot be
edited afterwards.

`portal::canonical_host::resolve_brand_and_enforce_host` resolves the key early in the middleware stack from the
incoming `Host:` header and stashes it as a request extension; `scope_branding` reads that extension and scopes the
resolved `Branding` for the rest of the request, the same [`views::brand::scope`](../../views/src/brand.rs) task-local
mechanism a mounted white-label bundle already used to scope its own `Branding`. An unregistered host redirects to the
deployment's own configured host (`CANONICAL_HOST`); the `/app/health` and `/app/readyz` probes answer on every host,
unredirected.

Public footer social links come from the request-scoped `Branding.firm_social` list. Each house Brand carries the same
firm YouTube channel; other destinations appear only on the Brand whose public profile was verified. Vesta carries its
Facebook, Instagram, Threads, and TikTok profiles. A renamed firm inherits none. These public links are presentation
metadata, separate from an authorized [Social Account](social-account.md): displaying a profile does not enable API
publishing.

Distinct from [`portal::hosting::Site`](../../portal/src/hosting.rs) (named apart from the per-request brand identity
above): a `Site` is what one brand *crate*'s `main` hands the shared run loop — its telemetry service name and the
public routes and Dioxus routers it composes. Two shapes ship:

- **`neon`** — [the whole site](../../neon/src/lib.rs), the `neon-server` image. It composes the public routes for every
  house brand the registry names and mounts the identical Navigator application beneath them, and it is the only binary
  that mounts the [Presentation](presentation.md) and workshop catalogs.
- **`tenant`** — [the white-label shape](../../portal/src/tenant.rs), which publishes no public face at all and
  redirects its bare host into the portal. It lives inside `portal` rather than in a crate of its own because a tenant
  has no public site to compose — that is the entire point.

Distinct again from [`views::brand::SiteBrand`](../../views/src/brand.rs) (`FIRM_BRAND`), the presentation half: the
strings, nav links, and footer attribution a page renders for whichever `Branding` is scoped to the request, overridable
by a mounted `BrandManifest` for the `neon` key alone. `BrandKey` names *which identity a request resolved to*, `Site`
names *the serving binary's own composition*, and `SiteBrand` names *what the page says*. A [Firm](firm.md) is none of
those three: it is the owning practice beneath the brand registry.

- Deployment map: [`environments`](../environments.md#why-the-brand-is-the-image)
